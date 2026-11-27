use super::*;
use crate::task_commands::agent_bench::{native_fixture, subject};
use af_contracts::task::execution::{TaskInvocationV1, TaskOutputV1};
use af_contracts::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use af_graph::RunReport;
use af_graph::task::CompiledNode;
use af_runner_codex::navigation::{
    self as codex, Bindings, CallIntent, DurableAck, NativeHooks, RawCallRecord,
};
use af_runtime::task::source::{invocation_producer, source_snapshot};
use af_runtime::task::{TaskOperatorHost, TaskWorkOutput};
use af_store::store::task::execution::{PreparedTaskAttempt, ReservedTaskAttempt};
use af_store::store::{TaskAttemptWall, task::task_run_id};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub(super) const CONFIG: &str = "af/AgentBenchExternalBindings@2";
pub(super) const SLOT: &str = "af/AgentBenchExternalSlot@1";
pub(super) const RECEIPT: &str = "af/AgentBenchExternalReceipt@1";
pub(super) const INCLUSIVE_LIMIT: u64 = 100_000;
pub(super) fn subject_wall_ms(phase: Phase) -> u64 {
    // The Task graph protects each slot's wall before admission. The full schedule
    // needs a smaller per-slot reservation to fit all 72 slots within eight hours.
    (if phase == Phase::Smoke { 10 } else { 6 }) * 60 * 1000
}
pub(super) const LIVE_DESTINATION: &str = "codex-personal";
pub(super) const FIXTURE_DESTINATION: &str = "native-dummy-responses";

const ANSWER_LIMIT: usize = 64 * 1024;

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BindingsInput {
    executable: PathBuf,
    executable_sha256: String,
    backend: Backend,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Backend {
    CodexPersonal {
        account_binding: String,
        auth_homes: Vec<PathBuf>,
        catalog: PathBuf,
        catalog_sha256: String,
    },
    LoopbackFixture {
        endpoint: std::net::SocketAddrV4,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Configuration {
    input: BindingsInput,
    declaration: InputPin,
    engine: String,
    catalog: String,
    catalog_sha256: String,
    analyzer: String,
    libraries: [String; 2],
    transport: String,
    broker: String,
    semantic: String,
}

impl Configuration {
    pub(super) fn capture(
        cas: &Cas,
        options: &BenchmarkPlan,
        repo: &Path,
        state: &Path,
        phase: Phase,
    ) -> Result<Self, String> {
        let external = &options.external;
        let declaration_bytes = read_bytes(
            external
                .transport
                .as_deref()
                .ok_or("Missing external transport")?,
            external
                .transport_sha256
                .as_deref()
                .ok_or("Missing external transport pin")?,
            INPUT_LIMIT,
        )?;
        let input: BindingsInput = serde_json::from_slice(&declaration_bytes)
            .map_err(|_| "Invalid external transport configuration")?;
        let meta = std::fs::symlink_metadata(&input.executable)
            .map_err(|_| "Native Codex executable metadata unavailable")?;
        if !input.executable.is_absolute() || !meta.is_file() {
            return Err("Native Codex requires an absolute regular executable".into());
        }
        read_bytes(
            &input.executable,
            &input.executable_sha256,
            512 * 1024 * 1024,
        )?;
        let catalog = match &input.backend {
            Backend::CodexPersonal {
                account_binding,
                auth_homes,
                catalog,
                catalog_sha256,
            } => {
                validate_homes(account_binding, auth_homes, phase.slots())?;
                for home in auth_homes {
                    auth_home(home, repo, state)?;
                }
                read_bytes(catalog, catalog_sha256, INPUT_LIMIT)?
            }
            Backend::LoopbackFixture { endpoint } => {
                if *endpoint.ip() != std::net::Ipv4Addr::LOCALHOST
                    || endpoint.port() == 0
                    || input.executable_sha256 != codex::LINUX_X86_64_SHA256
                {
                    return Err(
                        "Fixture requires IPv4 loopback and the pinned Linux Codex executable"
                            .into(),
                    );
                }
                codex::FIXTURE_MODEL_CATALOG.to_vec()
            }
        };
        codex::validate_catalog(&catalog)?;
        let analyzer_path = options
            .rust_analyzer
            .as_deref()
            .ok_or("Missing external rust-analyzer")?;
        if options.rust_analyzer_sha256.as_deref() != Some(semantic::LINUX_ANALYZER_SHA256) {
            return Err("External phase requires the pinned Linux rust-analyzer".into());
        }
        let library_dir = options
            .rust_analyzer_library_dir
            .as_deref()
            .ok_or("Missing analyzer libraries")?;
        let retain = |bytes: &[u8]| cas.put(bytes).map_err(|e| e.to_string());
        let analyzer = retain(&read_bytes(
            analyzer_path,
            semantic::LINUX_ANALYZER_SHA256,
            semantic::MAX_BINARY,
        )?)?;
        let [driver, llvm] = semantic::LINUX_LIBRARIES.map(|(name, digest, limit)| {
            retain(&read_bytes(&library_dir.join(name), digest, limit)?)
        });
        Ok(Self {
            input,
            declaration: InputPin {
                sha256: file_sha256(&declaration_bytes),
                object_id: retain(&declaration_bytes)?,
            },
            engine: engine(cas)?,
            catalog: retain(&catalog)?,
            catalog_sha256: codex::sha256(&catalog),
            analyzer,
            libraries: [driver?, llvm?],
            transport: codex::config_identity(),
            broker: navigation::Broker::conformance_identity(),
            semantic: semantic::config_identity(),
        })
    }

    pub(super) fn references(&self) -> Vec<String> {
        [
            self.engine.clone(),
            self.catalog.clone(),
            self.analyzer.clone(),
            self.declaration.object_id.clone(),
        ]
        .into_iter()
        .chain(self.libraries.clone())
        .collect()
    }

    pub(super) fn mode(&self) -> &'static str {
        match self.input.backend {
            Backend::CodexPersonal { .. } => "codex_personal",
            Backend::LoopbackFixture { .. } => "loopback_fixture",
        }
    }

    pub(super) fn live(&self) -> bool {
        matches!(self.input.backend, Backend::CodexPersonal { .. })
    }

    pub(super) fn destination(&self) -> &'static str {
        if self.live() {
            LIVE_DESTINATION
        } else {
            FIXTURE_DESTINATION
        }
    }

    pub(super) fn auth_homes(&self) -> &[PathBuf] {
        match &self.input.backend {
            Backend::CodexPersonal { auth_homes, .. } => auth_homes,
            Backend::LoopbackFixture { .. } => &[],
        }
    }

    pub(super) fn compiler(
        &self,
        cas: &Cas,
        id: &str,
        phase: Phase,
    ) -> Result<TaskPlanCompiler, String> {
        if self.input != read_input::<BindingsInput>(cas, &self.declaration)? {
            return Err("External bindings differ from the exact declaration".into());
        }
        if self.engine != engine(cas)?
            || self.transport != codex::config_identity()
            || self.broker != navigation::Broker::conformance_identity()
            || self.semantic != semantic::config_identity()
        {
            return Err(
                "External bindings require the captured engine and tool configuration".into(),
            );
        }
        let catalog = cas
            .get_bounded(&self.catalog, INPUT_LIMIT)
            .map_err(|e| e.to_string())?;
        if codex::sha256(&catalog) != self.catalog_sha256
            || match &self.input.backend {
                Backend::CodexPersonal { catalog_sha256, .. } => {
                    catalog_sha256 != &self.catalog_sha256
                }
                Backend::LoopbackFixture { .. } => catalog != codex::FIXTURE_MODEL_CATALOG,
            }
        {
            return Err("External catalog differs from its captured bytes".into());
        }
        codex::validate_catalog(&catalog)?;
        if let Backend::CodexPersonal {
            account_binding,
            auth_homes,
            ..
        } = &self.input.backend
        {
            validate_homes(account_binding, auth_homes, phase.slots())?;
        }
        compile_slots(
            cas,
            &self.engine,
            id,
            SlotPipeline {
                kind: KIND,
                slot_type: SLOT,
                receipt_type: RECEIPT,
                obligation: "collected",
                bindings_type: Some(CONFIG),
                count: phase.slots(),
                tokens: INCLUSIVE_LIMIT,
                wall_ms: subject_wall_ms(phase),
            },
        )
    }

    pub(super) fn same_protocol(&self, other: &Self) -> bool {
        self.live() == other.live()
            && self.input.executable_sha256 == other.input.executable_sha256
            && self.catalog_sha256 == other.catalog_sha256
            && self.engine == other.engine
            && self.analyzer == other.analyzer
            && self.libraries == other.libraries
            && self.transport == other.transport
            && self.broker == other.broker
            && self.semantic == other.semantic
            && match (&self.input.backend, &other.input.backend) {
                (
                    Backend::CodexPersonal {
                        account_binding: a, ..
                    },
                    Backend::CodexPersonal {
                        account_binding: b, ..
                    },
                ) => a == b,
                (Backend::LoopbackFixture { .. }, Backend::LoopbackFixture { .. }) => true,
                _ => false,
            }
    }

    fn bindings(&self, plan: &ExternalPlan, slot: &ExternalSlot, attempt: String) -> Bindings {
        Bindings {
            task_attempt_invocation: attempt,
            source: plan.pins.source_digest.clone(),
            prompt: codex::prompt_identity(&prompt(plan, slot)),
            tools: slot.tool_set_id.clone(),
            model_catalog: self.catalog_sha256.clone(),
            executable: self.input.executable_sha256.clone(),
            account: match &self.input.backend {
                Backend::CodexPersonal {
                    account_binding, ..
                } => account_binding.clone(),
                Backend::LoopbackFixture { .. } => codex::FIXTURE_ACCOUNT.into(),
            },
            protocol: codex::PROTOCOL.into(),
            config: self.transport.clone(),
        }
    }
}

fn validate_homes(account: &str, homes: &[PathBuf], count: usize) -> Result<(), String> {
    if !af_contracts::is_digest(account)
        || account == codex::FIXTURE_ACCOUNT
        || homes.len() != count
        || homes.iter().collect::<BTreeSet<_>>().len() != count
    {
        return Err(
            "Codex Personal requires an opaque account digest and one dedicated home per slot"
                .into(),
        );
    }
    Ok(())
}

pub(super) fn auth_home(home: &Path, repo: &Path, state: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if !home.is_absolute()
        || std::fs::canonicalize(home).ok().as_deref() != Some(home)
        || home.starts_with(repo)
        || home.starts_with(state)
    {
        return Err(
            "Native auth home must be a dedicated absolute directory outside source and Store"
                .into(),
        );
    }
    let meta = std::fs::symlink_metadata(home).map_err(|_| "Native auth home unavailable")?;
    let auth = std::fs::symlink_metadata(home.join("auth.json"))
        .map_err(|_| "Native auth metadata unavailable")?;
    if !meta.is_dir()
        || !auth.is_file()
        || auth.nlink() != 1
        || meta.permissions().mode() & 0o077 != 0
        || auth.permissions().mode() & 0o077 != 0
        || std::fs::read_dir(home)
            .map_err(|_| "Native auth home unavailable")?
            .count()
            != 1
    {
        return Err("Native auth home must contain only a private regular auth.json".into());
    }
    Ok(())
}

fn prompt(plan: &ExternalPlan, slot: &ExternalSlot) -> String {
    format!("{}\n\n{}", plan.common_instruction, slot.question)
}

pub(super) fn attempt_binding(task: &str, attempt: &str, context: &str) -> String {
    format!("{task}:{attempt}:{context}")
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    answer: Option<String>,
    failure: Option<String>,
    elapsed_ns: u64,
    inclusive_native_tokens: Option<u64>,
    cumulative_inclusive_tokens: Option<u64>,
    calls: Vec<String>,
    trace: Vec<subject::Trace>,
}

#[derive(Clone, Default)]
pub(super) struct Usage {
    pub(super) inclusive: u64,
    pub(super) counters: Option<TaskTokenUsageV3>,
    pub(super) missing: bool,
}

impl Usage {
    fn add(&mut self, raw: &RawCallRecord, live: bool) -> Result<(), String> {
        if raw.intent.route == "/models" {
            return Ok(());
        }
        let Some(usage) = raw.usage.as_ref().and_then(|u| u.reconciled.as_ref()) else {
            self.missing = true;
            return Ok(());
        };
        if usage.input.checked_add(usage.output) != Some(usage.inclusive)
            || usage
                .input
                .checked_sub(usage.cached_input)
                .and_then(|input| input.checked_add(usage.output))
                != Some(usage.chargeable)
            || usage
                .reasoning_output
                .is_some_and(|amount| amount > usage.output)
        {
            return Err("Contradictory external usage counters".into());
        }
        self.inclusive = self
            .inclusive
            .checked_add(usage.inclusive)
            .ok_or("Inclusive usage overflow")?;
        let counters = self.counters.get_or_insert_with(TaskTokenUsageV3::default);
        let add = |target: &mut Option<af_contracts::task::usage::DecimalU128>, amount: u64| {
            *target = Some((target.unwrap_or_default().get() + u128::from(amount)).into());
        };
        add(&mut counters.input_tokens, usage.input);
        add(&mut counters.output_tokens, usage.output);
        add(&mut counters.cache_read_tokens, usage.cached_input);
        if live {
            counters.chargeable_tokens =
                (counters.chargeable_tokens.get() + u128::from(usage.chargeable)).into();
        }
        Ok(())
    }

    pub(super) fn complete(&self) -> Option<u64> {
        (!self.missing && self.counters.is_some()).then_some(self.inclusive)
    }
}

pub(super) struct Hooks<'a> {
    pub(super) cas: &'a Cas,
    pub(super) database: &'a Path,
    pub(super) attempt: &'a PreparedTaskAttempt,
    pub(super) bindings: &'a Bindings,
    pub(super) cancelled: Option<&'a AtomicBool>,
    pub(super) live: bool,
    pub(super) cap: u64,
    pub(super) start: Instant,
    pub(super) wall: TaskAttemptWall,
    pub(super) state: Mutex<SendState>,
}

#[derive(Default)]
pub(super) struct SendState {
    pub(super) pending: Option<CallIntent>,
    pub(super) ids: Vec<String>,
    pub(super) usage: Usage,
    pub(super) failed: bool,
    pub(super) ordinal: u32,
}

impl Hooks<'_> {
    fn retain(&self, value: &impl Serialize) -> Result<String, String> {
        let id = self
            .cas
            .put_json(&serde_json::to_value(value).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        self.cas.flush().map_err(|e| e.to_string())?;
        Ok(id)
    }
}

impl NativeHooks for Hooks<'_> {
    fn before_send(&self, intent: &CallIntent, deadline: Instant) -> Result<DurableAck, String> {
        let mut state = self.state.lock().unwrap();
        if Instant::now() >= deadline
            || self
                .cancelled
                .is_some_and(|flag| flag.load(Ordering::Acquire))
            || intent.bindings != *self.bindings
            || intent.ordinal != state.ordinal + 1
            || intent.ordinal > native_fixture::MAX_SENDS
            || state.pending.is_some()
            || state.failed
            || state.usage.missing
            || state.usage.inclusive >= self.cap
        {
            return Err("Send lacks a current external Attempt allowance".into());
        }
        let id = self.retain(intent)?;
        let store = EventStore::open(self.database).map_err(|e| e.to_string())?;
        store
            .record_task_attempt_send(self.cas, self.attempt, intent.ordinal, &intent.id, &id)
            .map_err(|e| e.to_string())?;
        state.ids.push(id);
        state.ordinal = intent.ordinal;
        state.pending = Some(intent.clone());
        Ok(DurableAck {
            intent_id: intent.id.clone(),
        })
    }

    fn collect(&self, record: &RawCallRecord) -> Result<DurableAck, String> {
        let mut state = self.state.lock().unwrap();
        if state.pending.as_ref() != Some(&record.intent) {
            return Err("Response differs from the admitted external send".into());
        }
        let raw = self.retain(record)?;
        let mut usage = state.usage.clone();
        usage.add(record, self.live)?;
        let store = EventStore::open(self.database).map_err(|e| e.to_string())?;
        store
            .record_task_attempt_send_response(
                self.cas,
                self.attempt,
                &TaskAttemptWall {
                    usage: usage.counters.clone(),
                    elapsed_ms: self.start.elapsed().as_millis() as u64,
                    ..self.wall.clone()
                },
                &record.intent.id,
                &raw,
                !usage.missing,
            )
            .map_err(|e| e.to_string())?;
        state.usage = usage;
        state.ids.push(raw);
        state.pending = None;
        state.failed = record.failure.is_some();
        Ok(DurableAck {
            intent_id: record.intent.id.clone(),
        })
    }
}

pub(super) struct Domain {
    pub(super) prior_overhead: phase_evidence::Overhead,
    pub(super) plan: ExternalPlan,
    pub(super) configuration: Configuration,
    pub(super) graph: CompiledTask,
    pub(super) database: PathBuf,
    pub(super) repo: PathBuf,
}

impl Domain {
    fn slot(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<&ExternalSlot, String> {
        let id = input
            .inputs
            .get("slot")
            .and_then(|p| p.artifact_ids.first())
            .ok_or("Missing external slot")?;
        let given: ExternalSlot = artifact(cas, id, SLOT)?;
        let slot = self
            .plan
            .slots
            .get(given.index)
            .ok_or("Slot outside external smoke plan")?;
        if *slot != given || !input.node.ends_with(&format!(".{}", node_name(slot.index))) {
            return Err("Invocation changed its frozen external slot".into());
        }
        Ok(slot)
    }

    fn prior_usage(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<u64, String> {
        let slot = self.slot(cas, input)?;
        let used = if slot.index == 0 {
            0
        } else {
            let id = input
                .inputs
                .get("previous")
                .and_then(|port| port.artifact_ids.first())
                .ok_or("Previous external slot has no receipt")?;
            let receipt: Receipt = artifact(cas, id, RECEIPT)?;
            receipt
                .cumulative_inclusive_tokens
                .ok_or("Previous external usage is unknown")?
        };
        Ok(used)
    }

    fn run_subject(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        cancellation: Option<&AtomicBool>,
        observed: &mut SendState,
    ) -> Result<ArtifactInputV1, String> {
        let slot = self.slot(cas, input)?;
        let prior = self.prior_usage(cas, input)?;
        let cap = INCLUSIVE_LIMIT;
        let start = Instant::now();
        let started_unix_ms = clock()?;
        let deadline = start
            + Duration::from_millis(
                attempt
                    .reservation()
                    .deadline_unix_ms
                    .saturating_sub(started_unix_ms),
            );
        if attempt.reservation().tokens != INCLUSIVE_LIMIT {
            return Err("External Attempt does not hold the pinned allowance".into());
        }
        let (snapshot, _, manifest) = source_snapshot(cas, &input.inputs["source"])?;
        let broker =
            navigation::Broker::from_manifest(&manifest, cas).map_err(|e| e.to_string())?;
        if broker.source_identity() != self.plan.pins.source_digest {
            return Err("External broker source differs from phase plan".into());
        }
        let tool_set = if slot.tool_set_id == ToolSet::Text.identity() {
            ToolSet::Text
        } else {
            ToolSet::Semantic
        };
        let project: Vec<semantic::Crate> = read_input(cas, &self.plan.pins.project_graph)?;
        let runtime = (tool_set == ToolSet::Semantic)
            .then(|| -> Result<_, String> {
                let analyzer = cas
                    .get_bounded(&self.configuration.analyzer, semantic::MAX_BINARY)
                    .map_err(|e| e.to_string())?;
                let [driver, llvm] = semantic::LINUX_LIBRARIES.map(|(_, _, limit)| limit);
                let [driver, llvm] = [driver, llvm]
                    .into_iter()
                    .zip(&self.configuration.libraries)
                    .map(|(limit, id)| cas.get_bounded(id, limit).map_err(|e| e.to_string()))
                    .collect::<Result<Vec<_>, _>>()?
                    .try_into()
                    .map_err(|_| "Missing analyzer libraries")?;
                Ok((analyzer, driver, llvm))
            })
            .transpose()?;
        let analyzer = runtime
            .as_ref()
            .map(|(bytes, driver, llvm)| subject::Analyzer {
                bytes,
                digest: semantic::LINUX_ANALYZER_SHA256,
                project: &project,
                libraries: Some([driver, llvm]),
            });
        let mut trace = Vec::new();
        let mut tools = subject::Tools {
            arm: if analyzer.is_some() {
                Arm::Treatment
            } else {
                Arm::Baseline
            },
            broker,
            deadline,
            cancellation,
            trace: &mut trace,
        };
        let host = native_fixture::Host {
            tools: Mutex::new(&mut tools),
            analyzer,
        };
        let configuration = &self.configuration;
        let bindings = configuration.bindings(
            &self.plan,
            slot,
            attempt_binding(attempt.task_id(), attempt.id(), attempt.context_id()),
        );
        let hooks = Hooks {
            cas,
            attempt,
            database: &self.database,
            live: configuration.live(),
            cancelled: cancellation,
            start,
            cap,
            bindings: &bindings,
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
            profile: codex::WorkerProfile::Subject,
            pin: codex::Pin {
                executable: configuration.input.executable.clone(),
                executable_sha256: configuration.input.executable_sha256.clone(),
                catalog: cas
                    .get_bounded(&configuration.catalog, INPUT_LIMIT)
                    .map_err(|e| e.to_string())?,
                catalog_sha256: configuration.catalog_sha256.clone(),
            },
            native_auth_home: PathBuf::new(),
            bindings: bindings.clone(),
            question: prompt(&self.plan, slot),
            tools: tool_set,
            deadline,
            max_inclusive_tokens: cap,
            max_sends: native_fixture::MAX_SENDS,
        };
        let outcome = match &configuration.input.backend {
            Backend::CodexPersonal { auth_homes, .. } => {
                let home = auth_homes
                    .get(slot.index)
                    .ok_or("Missing dedicated subject home")?;
                auth_home(
                    home,
                    &self.repo,
                    self.database.parent().ok_or("Missing Store directory")?,
                )?;
                request.native_auth_home = home.clone();
                codex::run(request, &host, &hooks)
            }
            Backend::LoopbackFixture { endpoint } => {
                codex::run_fixture(request, *endpoint, &host, &hooks)
                    .map_err(|_| "External loopback fixture could not start")?
            }
        };
        let mut state = hooks.state.into_inner().unwrap();
        state.usage.missing |= state.pending.is_some();
        let inclusive = state.usage.complete();
        let answer = outcome
            .answer
            .filter(|answer| nonempty(answer, ANSWER_LIMIT));
        let failure = outcome.failure.or_else(|| {
            (!outcome.cleanup_confirmed || inclusive.is_none_or(|n| n > cap) || answer.is_none())
                .then_some("External answer lacks bounded usage, content or cleanup".into())
        });
        let mut refs = subject::input_refs(input);
        refs.extend(state.ids.clone());
        refs.sort();
        refs.dedup();
        let receipt = Receipt {
            answer,
            failure: failure.clone(),
            elapsed_ns: u64::try_from(start.elapsed().as_nanos()).map_err(|e| e.to_string())?,
            inclusive_native_tokens: inclusive,
            cumulative_inclusive_tokens: inclusive.map(|n| prior + n),
            calls: state.ids.clone(),
            trace,
        };
        let id = cas
            .put_artifact(
                RECEIPT,
                invocation_producer(cas, input, Some(attempt))?,
                refs,
                Some(snapshot.clone()),
                serde_json::to_value(receipt).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string());
        if let Ok((id, _)) = &id {
            state.ids.push(id.clone());
        }
        *observed = state;
        let (id, _) = id?;
        if let Some(failure) = failure {
            return Err(failure);
        }
        Ok(ArtifactInputV1 {
            artifact_type: RECEIPT.into(),
            cardinality: PortCardinality::One,
            artifact_ids: vec![id],
            snapshot_id: Some(snapshot),
        })
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
        self.prior_usage(cas, input)?;
        let store = EventStore::open_read_only(&self.database).map_err(|e| e.to_string())?;
        let task = store
            .task_projection(cas, attempt.task_id())
            .map_err(|e| e.to_string())?
            .ok_or("External subject has no Task")?;
        let clock_start = task
            .clock_started_unix_ms()
            .ok_or("External phase has no admitted clock")?;
        let total_wall = clock()?.saturating_sub(clock_start);
        let mut subject_wall = 0_u64;
        if let Some(run) = &task.execution {
            for attempt in run.attempt_accounting().into_iter().filter(|a| a.started) {
                for id in &attempt.raw_artifact_ids {
                    let envelope = cas.get_optional_artifact(id).map_err(|e| e.to_string())?;
                    if let Some(envelope) = envelope.filter(|a| a.artifact_type == RECEIPT) {
                        let receipt: Receipt =
                            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
                        subject_wall = subject_wall
                            .checked_add(receipt.elapsed_ns / 1_000_000)
                            .ok_or("Subject wall overflow")?;
                    }
                }
            }
        }
        if self.prior_overhead.tokens >= grading::OVERHEAD_TOKEN_LIMIT
            || self
                .prior_overhead
                .wall_ms
                .saturating_add(total_wall.saturating_sub(subject_wall))
                >= grading::OVERHEAD_WALL_MS
        {
            return Err("External pilot overhead allowance exhausted".into());
        }
        if task.execution.as_ref().is_none_or(|run| {
            run.attempt_accounting()
                .iter()
                .any(|row| row.started && row.reservation.node == input.node)
        }) {
            return Err("External slot is single-use".into());
        }
        cas.put_json(&serde_json::to_value(input).map_err(|e| e.to_string())?)
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
        let mut state = SendState::default();
        let outputs = attempt
            .ok_or("External subject requires a native Attempt".into())
            .and_then(|attempt| self.run_subject(cas, input, attempt, cancellation, &mut state))
            .map(|port| BTreeMap::from([("receipt".into(), port)]));
        let sent = state.ordinal != 0;
        let charge_complete = state.usage.complete().is_some();
        let usage = state.usage.counters;
        TaskWorkOutput {
            outputs,
            notes: Default::default(),
            usage_observation: (self.configuration.live() && sent).then(|| {
                TaskUsageObservationV1 {
                    charge_complete,
                    reported_usage: usage.clone(),
                }
            }),
            charged_tokens: (!self.configuration.live() || !sent).then_some(0),
            usage,
            raw_artifact_ids: state.ids,
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
        self.prior_usage(cas, input)?;
        if cas.get_json(id).map_err(|e| e.to_string())?
            != serde_json::to_value(input).map_err(|e| e.to_string())?
        {
            return Err("External context differs from its exact invocation".into());
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
        let prior = self.prior_usage(cas, input)?;
        let cap = INCLUSIVE_LIMIT;
        let id = &output.outputs["receipt"].artifact_ids[0];
        let receipt: Receipt = artifact(cas, id, RECEIPT)?;
        let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
        let Producer::Attempt { attempt_id, .. } = envelope.producer else {
            return Err("External receipt has no Attempt producer".into());
        };
        let context =
            af_store::content_id(&serde_json::to_value(input).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let expected = self.configuration.bindings(
            &self.plan,
            slot,
            attempt_binding(&task.task_id, &attempt_id, &context),
        );
        let (_, usage) = replay_calls(
            cas,
            receipt
                .calls
                .chunks(2)
                .map(|ids| (ids[0].clone(), ids.get(1).cloned())),
            &expected,
            self.configuration.live(),
        )?;
        let mut refs = subject::input_refs(input);
        refs.extend(receipt.calls);
        refs.sort();
        refs.dedup();
        if envelope.input_artifacts != refs
            || receipt.failure.is_some()
            || receipt
                .answer
                .as_deref()
                .is_none_or(|answer| !nonempty(answer, ANSWER_LIMIT))
            || usage.complete() != receipt.inclusive_native_tokens
            || usage
                .complete()
                .is_none_or(|n| n > cap || Some(prior + n) != receipt.cumulative_inclusive_tokens)
        {
            return Err("External receipt differs from its exact invocation or usage".into());
        }
        Ok(())
    }

    fn assemble_result(
        &self,
        _: &Cas,
        task: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let mut result = subject::collection_result(&self.graph, task, report)?;
        self.assess(&mut result);
        Ok(result)
    }

    fn validate_result(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let mut expected = result.clone();
        self.assess(&mut expected);
        if expected != *result {
            return Err(
                "External collection result differs from its admitted subject receipts".into(),
            );
        }
        Ok(())
    }
}

impl Domain {
    fn assess(&self, result: &mut TaskResultV1) {
        // The execution admits a receipt only for a collected answer with bounded usage.
        let complete = result.execution == TaskExecutionV1::Completed
            && self
                .plan
                .slots
                .iter()
                .all(|slot| result.outputs.contains_key(&node_name(slot.index)));
        result.missing_obligations.clear();
        result.acceptance = if complete {
            TaskAcceptanceV1::Satisfied
        } else {
            result.missing_obligations.insert("collected".into());
            if result.execution == TaskExecutionV1::Completed {
                result.execution = TaskExecutionV1::Incomplete;
            }
            TaskAcceptanceV1::Inconclusive
        };
        result.domain_conclusion = match (self.configuration.live(), complete) {
            (true, true) => "external_live_subjects_collected",
            (false, true) => "external_fixture_subjects_collected",
            (true, false) => "external_live_collection_incomplete",
            (false, false) => "external_fixture_collection_incomplete",
        }
        .into();
    }
}

pub(super) fn read_configuration(
    cas: &Cas,
    task: &TaskProjection,
) -> Result<Option<Configuration>, String> {
    let Some(port) = task.revision.inputs.get("bindings") else {
        return Ok(None);
    };
    let [id] = port.artifact_ids.as_slice() else {
        return Err("External plan requires exactly one bindings artifact".into());
    };
    let configuration: Configuration = artifact(cas, id, CONFIG)?;
    if task.revision.authority.data_destinations
        != BTreeSet::from([configuration.destination().into()])
        || (configuration.live()
            && task.revision.inputs["source"].snapshot_id.as_deref() != Some(FROZEN_SNAPSHOT))
    {
        return Err("External configuration changed the captured destination".into());
    }
    Ok(Some(configuration))
}

pub(super) fn replay_calls(
    cas: &Cas,
    rows: impl Iterator<Item = (String, Option<String>)>,
    expected: &Bindings,
    live: bool,
) -> Result<(Vec<serde_json::Value>, Usage), String> {
    let mut calls = Vec::new();
    let mut usage = Usage::default();
    let mut ids = BTreeSet::new();
    let mut failed = false;
    for (index, (intent_id, raw_id)) in rows.enumerate() {
        let intent: CallIntent =
            serde_json::from_value(cas.get_json(&intent_id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if intent.bindings != *expected
            || intent.ordinal as usize != index + 1
            || intent.ordinal > native_fixture::MAX_SENDS
            || !ids.insert(intent.id.clone())
            || !af_contracts::is_digest(&intent.request_sha256)
            || !matches!(intent.route.as_str(), "/models" | "/responses")
            || usage.missing
            || failed
        {
            return Err("Retained send differs from its external Attempt".into());
        }
        let raw = raw_id
            .as_ref()
            .map(|id| -> Result<RawCallRecord, String> {
                serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        if let Some(raw) = &raw {
            if raw.intent != intent
                || raw.drained_mono_ns < intent.started_mono_ns
                || !af_contracts::is_digest(&raw.response_sha256)
                || (intent.route == "/models" && raw.usage.is_some())
            {
                return Err("External response differs from its admitted intent".into());
            }
            usage.add(raw, live)?;
            failed = raw.failure.is_some();
        } else {
            usage.missing = true;
        }
        calls.push(json!({"intent_artifact_id": intent_id, "raw_artifact_id": raw_id, "intent": intent, "raw": raw}));
    }
    Ok((calls, usage))
}

pub(super) fn report_slots(
    cas: &Cas,
    store: &EventStore,
    task: &TaskProjection,
    plan: &ExternalPlan,
    configuration: &Configuration,
) -> Result<Vec<serde_json::Value>, String> {
    let run_id = task_run_id(&task.task_id).map_err(|e| e.to_string())?;
    let attempts = task
        .execution
        .as_ref()
        .map(|run| run.attempt_accounting())
        .unwrap_or_default();
    let mut slots = Vec::new();
    for slot in &plan.slots {
        let name = node_name(slot.index);
        let node = task
            .execution
            .as_ref()
            .and_then(|run| run.graph.outputs.get(&name))
            .map(|a| &a.node);
        let mut rows = Vec::new();
        let mut receipt = None;
        let mut receipt_id = None;
        let mut status = "unstarted";
        let mut inclusive = None;
        let mut charged = None;
        let mut started = false;
        for attempt in attempts
            .iter()
            .filter(|a| node == Some(&a.reservation.node))
        {
            let retained = store
                .task_attempt_sends(&run_id, &attempt.attempt_id)
                .map_err(|e| e.to_string())?;
            let (calls, usage) = if retained.is_empty() {
                (vec![], Usage::default())
            } else {
                let context = attempt
                    .context_id
                    .as_deref()
                    .ok_or("External send has no bound context")?;
                let expected = configuration.bindings(
                    plan,
                    slot,
                    attempt_binding(&task.task_id, &attempt.attempt_id, context),
                );
                replay_calls(cas, retained.into_iter(), &expected, configuration.live())?
            };
            require_settlement_links(attempt, &calls)?;
            for id in &attempt.raw_artifact_ids {
                let envelope = cas.get_optional_artifact(id).map_err(|e| e.to_string())?;
                if let Some(envelope) = envelope.filter(|e| e.artifact_type == RECEIPT) {
                    let producer = Producer::Attempt {
                        run_id: run_id.clone(),
                        node_id: attempt.reservation.node.clone(),
                        attempt_id: attempt.attempt_id.clone(),
                    };
                    if envelope.producer != producer || receipt.is_some() {
                        return Err(
                            "External report found a mismatched or duplicate receipt".into()
                        );
                    }
                    let result: Receipt =
                        serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
                    let linked: Vec<_> = calls
                        .iter()
                        .flat_map(|call| {
                            ["intent_artifact_id", "raw_artifact_id"]
                                .into_iter()
                                .filter_map(|field| call[field].as_str())
                        })
                        .collect();
                    if result.calls.iter().map(String::as_str).ne(linked)
                        || result.inclusive_native_tokens != usage.complete()
                    {
                        return Err("External receipt differs from the durable sends".into());
                    }
                    receipt = Some(result);
                    receipt_id = Some(id.clone());
                }
            }
            if attempt.started {
                if started {
                    return Err("External report found multiple starts for one subject slot".into());
                }
                started = true;
                inclusive = usage.complete();
                charged = attempt
                    .result
                    .as_ref()
                    .map(|_| attempt.charged_tokens.to_string());
                status = match (&attempt.result, &receipt) {
                    (
                        Some(af_contracts::task::execution::TaskAttemptResultV1::Succeeded {
                            ..
                        }),
                        Some(receipt),
                    ) if receipt.failure.is_none()
                        && inclusive.is_some_and(|n| n <= INCLUSIVE_LIMIT)
                        && attempt.settled_unix_ms.is_some() =>
                    {
                        "collected"
                    }
                    (Some(_), _) => "failed",
                    _ => "missing",
                };
            }
            rows.push(json!({
                "attempt_id": attempt.attempt_id, "started": attempt.started,
                "context_id": attempt.context_id, "reserved_tokens": attempt.reservation.tokens,
                "af_charged_tokens": attempt.result.as_ref().map(|_| attempt.charged_tokens.to_string()),
                "settlement": attempt.result, "abandoned": attempt.abandoned.map(|cause| cause.as_str()),
                "usage_id": attempt.usage_id, "unknown_usage": attempt.unknown_usage,
                "known_af_chargeable_tokens": usage.counters.as_ref().map(|c| c.chargeable_tokens),
                "known_inclusive_tokens": usage.counters.as_ref().map(|_| usage.inclusive),
                "inclusive_native_tokens": usage.complete(), "calls": calls,
            }));
        }
        let mut value = serde_json::to_value(slot).map_err(|e| e.to_string())?;
        let fields = value.as_object_mut().ok_or("Invalid external slot")?;
        fields.extend([
            ("status".into(), json!(status)),
            ("attempts".into(), json!(rows)),
            (
                "elapsed_ns".into(),
                json!(receipt.as_ref().map(|r| r.elapsed_ns)),
            ),
            ("receipt".into(), json!(receipt)),
            ("receipt_id".into(), json!(receipt_id)),
            ("grade".into(), serde_json::Value::Null),
            ("inclusive_native_tokens".into(), json!(inclusive)),
            ("charged_tokens".into(), json!(charged)),
        ]);
        slots.push(value);
    }
    Ok(slots)
}

pub(super) fn require_settlement_links(
    attempt: &af_store::store::task::execution::TaskAttemptAccounting,
    calls: &[serde_json::Value],
) -> Result<(), String> {
    if matches!(
        attempt.result,
        Some(af_contracts::task::execution::TaskAttemptResultV1::Succeeded { .. })
    ) && calls.iter().any(|call| {
        ["intent_artifact_id", "raw_artifact_id"]
            .into_iter()
            .filter_map(|field| call[field].as_str())
            .any(|id| !attempt.raw_artifact_ids.iter().any(|raw| raw == id))
    }) {
        return Err("A settled external Attempt lacks a retained send link".into());
    }
    Ok(())
}

pub(super) fn report(
    cas: &Cas,
    store: &EventStore,
    task: &TaskProjection,
    plan: &ExternalPlan,
    configuration: &Configuration,
    as_json: bool,
) -> Result<i32, String> {
    let mut slots = report_slots(cas, store, task, plan, configuration)?;
    for slot in &mut slots {
        slot.as_object_mut()
            .ok_or("Invalid external slot")?
            .remove("receipt_id");
    }
    if as_json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "schema": "af.agent-bench-external-report/3", "task_id": task.task_id,
            "plan_id": task.plan_id, "phase_plan_id": task.revision.authority.policy_id,
            "phase": plan.phase, "pins": plan.pins, "common_instruction": plan.common_instruction,
            "prerequisite": prerequisite(plan), "predecessor": plan.predecessor,
            "execution": configuration.mode(), "slots": slots
        })).map_err(|e| e.to_string())?);
    } else {
        println!(
            "Task {} · external {} · {}",
            task.task_id,
            plan.phase.label(),
            configuration.mode()
        );
        for slot in slots {
            println!("{} {} {}", slot["index"], slot["variant"], slot["status"]);
        }
    }
    Ok(0)
}
