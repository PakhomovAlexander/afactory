//! Scenario CLI parsing and presentation; the domain lives in `af_scenario`.
pub(crate) mod host;
use af_scenario::{
    contract, performance, roadmap, runtime, schedule, source, state, store, workspace,
};
use clap::{Args, Subcommand};
use contract::*;
use host::AfHost;
use schedule::*;
use state::*;
use std::path::PathBuf;
use std::sync::Arc;
use store::Store;

#[derive(Debug, Args)]
pub(crate) struct Inspect {
    /// Private Scenario journal directory, outside the checkout (default: XDG state/af/scenarios)
    #[arg(long, global = true)]
    state: Option<PathBuf>,
    /// Print one versioned JSON document
    #[arg(long, global = true)]
    json: bool,
    /// Explicit observation time in Unix milliseconds; absent, use the machine clock
    #[arg(long, global = true)]
    now: Option<u64>,
}
#[derive(Debug, Args)]
pub(crate) struct Definition {
    /// Versioned Scenario bundle or Ant definition (TOML authoring or JSON wire format)
    #[arg(long)]
    file: PathBuf,
    /// Owner-only machine policy outside the checkout; grants Task plans, templates and adapters
    #[arg(long)]
    authority: PathBuf,
}
#[derive(Debug, Subcommand)]
pub(crate) enum ScenarioCommand {
    /// Render a named recorded result as Markdown; observing never executes work
    Output {
        /// Scenario Run or Ant label, according to this command's namespace
        id: String,
        #[command(flatten)]
        view: OutputView,
        #[command(flatten)]
        inspect: Inspect,
    },

    /// Retrieve one provenance-checked native findings, History Capture or adoption artifact
    #[command(
        override_usage = "af scenario evidence-source <TASK_ID> --artifact-id ID\n                            --repo REPO --task-state STORE [OPTIONS]",
        after_long_help = "Examples:\n  # needs: a native artifact recorded by the selected Task\n  af scenario evidence-source TASK_ID --artifact-id sha256:... \\\n    --repo /project --task-state /private/tasks --json"
    )]
    EvidenceSource {
        /// Task whose recorded evidence authorizes the selected artifact
        task_id: String,
        /// Exact native artifact ID, not a path to an exported JSON file
        #[arg(long)]
        artifact_id: String,
        /// Repository the Task belongs to
        #[arg(long)]
        repo: PathBuf,
        /// Private native Task Store containing the artifact and source receipts
        #[arg(long)]
        task_state: PathBuf,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Validate native performance publication Tasks and render their exact report block
    #[command(
        long_about = "Read exact accepted implementation, independent verification and final report Tasks, check their source and measured comparison bindings, and render native publication reports. This read-only command grants no publication authority; the installed publication Action additionally re-observes the pinned native lab and enforces machine scope."
    )]
    PerformanceProof {
        /// af/performance-pr-proof-request@1 JSON naming exact Task and comparison receipts
        #[arg(long)]
        file: PathBuf,
        /// Repository whose native Task source and candidate provenance are checked
        #[arg(long)]
        repo: PathBuf,
        /// Private native Task Store holding accepted Tasks, Snapshots and report receipts
        #[arg(long)]
        task_state: PathBuf,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Export an ordinary accepted Task candidate as private, provenance-checked Git objects
    #[command(
        override_usage = "af scenario candidate-source --file FILE --repo REPO\n                             --task-state STORE --into DIR [OPTIONS]",
        after_long_help = "Examples:\n  # needs: an exact accepted Task receipt in a versioned request\n  af scenario candidate-source --file request.json --repo /project \\\n    --task-state /private/tasks --into /private/captures/candidate --json"
    )]
    CandidateSource {
        /// af/git-candidate-source-request@1 JSON containing the exact accepted Task receipt
        #[arg(long)]
        file: PathBuf,
        /// Repository whose Git history anchors the Task source
        #[arg(long)]
        repo: PathBuf,
        /// Private native Task Store holding the accepted result and Snapshots
        #[arg(long)]
        task_state: PathBuf,
        /// Private capture directory outside the repository and Task Store
        #[arg(long)]
        into: PathBuf,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Capture an accepted Task's exact Snapshots as private benchmark Git objects
    #[command(
        long_about = "Write a private experiment source representation from an exact accepted Task receipt. \
The request binds the Task result and its original source and candidate Snapshots. Native \
materialization writes their bytes without filters into private Git objects, with deterministic \
commits and retained Snapshot provenance. The destination must be absent, outside the repository \
and Task Store. Existing complete captures are verified and reused; incomplete captures refuse. \
No checkout is changed and nothing is published.",
        override_usage = "af scenario experiment-source --file FILE --repo REPO\n                              --task-state STORE --into DIR [OPTIONS]",
        after_long_help = "Examples:\n  # needs: an accepted Task receipt in a versioned source request\n  \
af scenario experiment-source --file source.json --repo /project \\\n    --task-state /private/tasks --into /private/experiments/source --json"
    )]
    ExperimentSource {
        /// af/scenario-experiment-source-request@1 JSON request with an exact accepted Task receipt
        #[arg(long)]
        file: PathBuf,
        /// Repository the Task belongs to
        #[arg(long)]
        repo: PathBuf,
        /// Private native Task Store holding the accepted result and source objects
        #[arg(long)]
        task_state: PathBuf,
        /// Absent private capture directory outside the repository and Task Store
        #[arg(long)]
        into: PathBuf,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Capture fresh Task bytes, native authority and source identities; launches no Worker
    #[command(
        override_usage = "af scenario template-pin --file TASK --repo REPO --task-state STORE\n                         --authority-commit COMMIT [OPTIONS]"
    )]
    TemplatePin {
        /// Native Task file to capture as a fresh Task template (TOML or JSON)
        #[arg(long)]
        file: PathBuf,
        /// Repository whose committed source and Task authority the template pins
        #[arg(long)]
        repo: PathBuf,
        /// Private Task Store directory outside the checkout for instantiated Tasks
        #[arg(long)]
        task_state: PathBuf,
        /// Exact 40-character commit supplying the template's source and Task authority
        #[arg(long)]
        authority_commit: String,
        /// Native bindings; captures pinned local Workers relative to this file once
        #[arg(long)]
        bindings: Option<PathBuf>,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Plan and retain goal-based optimization roadmaps; never dispatch Tasks or activate Ants
    Roadmap {
        #[command(subcommand)]
        command: RoadmapCommand,
    },
    /// Validate types, transitive definitions, authority and bounds; print the exact pin to review
    Validate {
        #[command(flatten)]
        definition: Definition,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Record a Run of the reviewed pin; --execute drives it until it settles or waits
    #[command(
        override_usage = "af scenario start <RUN_ID> --file FILE --authority POLICY\n                  --inputs FILE --confirm-pin REVISION [OPTIONS]"
    )]
    Start {
        /// Local Run label in the selected Scenario Store
        run_id: String,
        #[command(flatten)]
        definition: Definition,
        /// JSON object of concrete typed root inputs (receipts cannot be supplied as root inputs)
        #[arg(long)]
        inputs: PathBuf,
        /// Exact revision printed by validate; confirms the full definition and machine policy
        #[arg(long)]
        confirm_pin: String,
        /// Drive the recorded Run immediately until it settles or waits
        #[arg(long)]
        execute: bool,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Inspect a durable Run, including pinned definitions, intents, child progress and evidence
    Show {
        /// Local Run label in the selected Scenario Store
        run_id: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// List recorded Runs
    List {
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Resume the pinned Run, reconciling any uncertain effects before retrying
    Resume {
        /// Local Run label in the selected Scenario Store
        run_id: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Stop new dispatch and request active Task cancellation; retains effects and receipts
    Cancel {
        /// Local Run label in the selected Scenario Store
        run_id: String,
        /// Accountable cancellation reason retained with the Run
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Write exact signing bytes for the current decision; sign externally with minisign
    DecisionPayload {
        /// Local Run label in the selected Scenario Store
        run_id: String,
        /// Absent file to create with exact decision signing bytes
        #[arg(long)]
        out: PathBuf,
        /// Request a signed rejection instead of approval
        #[arg(long)]
        reject: bool,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Authenticate and record a signed decision JSON envelope; resume is a separate operation
    Decide {
        /// Local Run label in the selected Scenario Store
        run_id: String,
        /// Signed af/scenario-signed-decision@1 JSON envelope
        #[arg(long)]
        file: PathBuf,
        #[command(flatten)]
        inspect: Inspect,
    },
}
#[derive(Debug, Subcommand)]
pub(crate) enum AntCommand {
    /// Explicit foreground scheduler lifecycle; never installs a service
    Scheduler {
        #[command(subcommand)]
        command: SchedulerCommand,
    },

    /// Render a named recorded result as Markdown; observing never executes work
    Output {
        /// Scenario Run or Ant label, according to this command's namespace
        id: String,
        #[command(flatten)]
        view: OutputView,
        #[command(flatten)]
        inspect: Inspect,
    },

    /// Validate and pin the definition, machine policy and explicit timezone data
    Validate {
        #[command(flatten)]
        definition: Definition,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Register a reviewed Ant, initially disabled; this creates no cron job or daemon
    Add {
        #[command(flatten)]
        definition: Definition,
        /// Exact Ant revision printed by validate, including policy and timezone data
        #[arg(long)]
        confirm_pin: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Inspect the Ant, schedule cursor, spending reservations and occurrence outcomes
    Show {
        /// Ant label in the selected Scenario Store
        ant_id: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// List registered Ants
    List {
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Enable future occurrences; existing Runs keep their own lifecycle
    Enable {
        /// Ant label in the selected Scenario Store
        ant_id: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Pause future launches, retaining the existing active Run, including waits
    Pause {
        /// Ant label in the selected Scenario Store
        ant_id: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Disable future launches; cancelling the active Run is a separate operation
    Disable {
        /// Ant label in the selected Scenario Store
        ant_id: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Admit due occurrences and drive active Runs, reporting busy or refused bindings
    Tick {
        #[command(flatten)]
        inspect: Inspect,
    },
}
#[derive(Debug, Args)]
pub(crate) struct OutputView {
    /// Named Scenario result port (required when a Scenario has multiple outputs)
    #[arg(long)]
    name: Option<String>,
    /// Present already-authored text as Markdown (requires recorded observation metadata)
    #[arg(long, requires_all = ["name", "observed_output", "stale_after_ms"])]
    raw_markdown: bool,
    /// Named integer output containing the source observation Unix time in milliseconds
    #[arg(long, requires = "raw_markdown")]
    observed_output: Option<String>,
    /// Freshness lifetime of raw Markdown in milliseconds; never changes observation time
    #[arg(long, requires = "raw_markdown", value_parser = clap::value_parser!(u64).range(1..=af_scenario::contract::MAX_TIME))]
    stale_after_ms: Option<u64>,
    /// Repeated read-only snapshots until interrupted
    #[arg(long)]
    watch: bool,
    /// View polling cadence in integral seconds, independent of schedule/producer cadence
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..=86400))]
    interval_seconds: u64,
    /// Atomically replace this Markdown file; otherwise stream bounded Markdown snapshots
    #[arg(long)]
    output: Option<PathBuf>,
}
#[derive(Debug, Subcommand)]
pub(crate) enum SchedulerCommand {
    /// Run in the foreground until stop, SIGINT or SIGTERM; owns one Store lease
    Run {
        /// Optional finite lifecycle for private fixtures and supervision
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        for_seconds: Option<u64>,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Request graceful stop after current bounded dispatch; leaves Run state recoverable
    Stop {
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Inspect the live OS lease and last heartbeat, including a crashed scheduler
    Status {
        #[command(flatten)]
        inspect: Inspect,
    },
}
fn scheduler(command: SchedulerCommand) -> Result<i32> {
    match command {
        SchedulerCommand::Status { inspect } => {
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            print(&store.scheduler_status()?, &inspect)
        }
        SchedulerCommand::Stop { inspect } => {
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            store.scheduler_stop()?;
            print(&store.scheduler_status()?, &inspect)
        }
        SchedulerCommand::Run {
            for_seconds,
            inspect,
        } => {
            require(
                inspect.now.is_none(),
                "Continuous scheduler requires the real clock",
            )?;
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            let _lease = store.scheduler_start(runtime::now()?)?;
            let started = std::time::Instant::now();
            let mut last_error: Option<String> = None;
            let mut had_errors = false;
            loop {
                if crate::interrupt::received().is_some() {
                    break;
                }
                if for_seconds.is_some_and(|s| started.elapsed().as_secs() >= s) {
                    break;
                }
                let at = runtime::now()?;
                if store.scheduler_heartbeat(at, last_error.as_deref())? {
                    break;
                }
                let poll = std::time::Instant::now();
                // Native supervised Task/adapter dispatch retains its existing deadlines,
                // reconciliation and per-Run locks. Failed ticks are visible, never successes.
                let error = match runtime::tick(&store, at, None) {
                    Ok(tick) if tick.refused.is_empty() => None,
                    Ok(tick) => {
                        Some(serde_json::to_string(&tick.refused).map_err(|e| e.to_string())?)
                    }
                    Err(error) => {
                        // A global journal/dispatch failure is not a deferred Ant. Keep its
                        // diagnostic and exit; never hot-loop a full or damaged Store.
                        store.scheduler_heartbeat(runtime::now()?, Some(&error))?;
                        return Err(error);
                    }
                };
                if let Some(error) = &error {
                    had_errors = true;
                    eprintln!("af scheduler: {error}");
                }
                last_error = error.clone();
                if store.scheduler_heartbeat(runtime::now()?, error.as_deref())? {
                    break;
                }
                crate::interrupt::wait(
                    std::time::Duration::from_secs(1).saturating_sub(poll.elapsed()),
                );
            }
            Ok(i32::from(had_errors))
        }
    }
}
fn output(id: &str, is_ant: bool, view: &OutputView, inspect: &Inspect) -> Result<i32> {
    use af_scenario::presentation::{Snapshot, atomic_write};
    use std::io::Write;
    require(
        !inspect.json,
        "Output emits Markdown; use show --json for complete underlying native results",
    )?;
    require(
        !view.watch || inspect.now.is_none(),
        "Watch requires a real render clock",
    )?;
    let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
    let mut output_name = view.name.clone();
    loop {
        if crate::interrupt::received().is_some() {
            break;
        }
        let now = at(inspect)?;
        let result = (|| -> Result<Snapshot> {
            let recorded = store.read_output(is_ant, id)?;
            let selected = match &view.name {
                Some(name) => name.as_str(),
                None if recorded.outputs.len() == 1 => {
                    recorded.outputs.keys().next().unwrap().as_str()
                }
                None => {
                    return Err(
                        "Multiple named outputs: select --name; inspect show --json for names"
                            .into(),
                    );
                }
            };
            // Observe render time after the indexed read: a concurrent producer can commit
            // an observation after this refresh began, without its clock being in the future.
            if view.raw_markdown {
                recorded.markdown_snapshot(
                    selected,
                    view.observed_output
                        .as_deref()
                        .ok_or("Missing observation output")?,
                    view.stale_after_ms.ok_or("Missing freshness bound")?,
                    at(inspect)?,
                )
            } else {
                recorded.snapshot(selected, at(inspect)?)
            }
        })();
        let failed = result.is_err();
        if let Ok(snapshot) = &result {
            output_name = Some(snapshot.output.clone());
        }
        let snapshot = result.unwrap_or_else(|error| {
            let mut snapshot = Snapshot::unavailable(
                output_name.as_deref().unwrap_or("unselected"),
                now,
                Some(error),
            );
            if is_ant {
                snapshot.ant = Some(id.into());
            } else {
                snapshot.run = Some(id.into());
            }
            snapshot
        });
        let markdown = snapshot.markdown()?;
        if let Some(path) = &view.output {
            atomic_write(path, &markdown)?;
        } else {
            let mut stdout = std::io::stdout().lock();
            if let Err(e) = stdout
                .write_all(markdown.as_bytes())
                .and_then(|()| stdout.flush())
            {
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    return Ok(0);
                }
                return Err(e.to_string());
            }
        }
        if !view.watch {
            return Ok(if failed { 1 } else { 0 });
        }
        crate::interrupt::wait(std::time::Duration::from_secs(view.interval_seconds));
    }
    Ok(0)
}
fn pin(definition: &Definition) -> Result<Pin> {
    let mut bundle: Bundle = store::definition(&definition.file)?;
    normalize_targets(&mut bundle)?;
    Pin::new(&AfHost, bundle, store::policy(&definition.authority)?)
}
fn ant_pin(definition: &Definition) -> Result<AntPin> {
    let mut ant: Ant = store::definition(&definition.file)?;
    normalize_targets(&mut ant.scenario)?;
    AntPin::new(&AfHost, ant, store::policy(&definition.authority)?)
}
fn normalize_targets(bundle: &mut Bundle) -> Result<()> {
    for def in bundle.definitions.values_mut() {
        for step in &mut def.steps {
            if let Kind::Task { target } = &mut step.operation {
                require(
                    target.repo.is_absolute() && target.state.is_absolute(),
                    "Task repository and Store paths must be absolute",
                )?;
                target.repo = crate::resolve_filesystem_path(&target.repo)?;
                target.state = crate::resolve_filesystem_path(&target.state)?;
            }
        }
    }
    Ok(())
}
fn print(value: &impl serde::Serialize, inspect: &Inspect) -> Result<i32> {
    let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
    if inspect.json {
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|e| e.to_string())?
        );
    } else {
        let kind = value["schema"].as_str().unwrap_or("Scenario");
        if let Some(status) = value.get("status") {
            println!(
                "{}: {}",
                value["id"].as_str().unwrap_or("Run"),
                status.as_str().unwrap_or("unknown")
            );
        } else if let Some(revision) = value.get("revision") {
            println!("{kind}: {}", revision.as_str().unwrap_or("unknown"));
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
            );
        }
        for key in ["waiting", "failure"] {
            if value.get(key).is_some_and(|v| !v.is_null()) {
                println!("{key}: {}", value[key]);
            }
        }
    }
    Ok(0)
}
fn at(inspect: &Inspect) -> Result<u64> {
    let t = inspect.now.map_or_else(runtime::now, Ok)?;
    require(
        t > 0 && t <= MAX_TIME,
        "Observation time outside the supported range",
    )?;
    Ok(t)
}
pub(crate) fn scenario(command: ScenarioCommand) -> Result<i32> {
    match command {
        ScenarioCommand::Output { id, view, inspect } => output(&id, false, &view, &inspect),
        ScenarioCommand::PerformanceProof {
            file,
            repo,
            task_state,
            inspect,
        } => {
            let request: performance::ProofRequest =
                serde_json::from_slice(&store::read(&file, MAX_BYTES)?)
                    .map_err(|e| e.to_string())?;
            let proof =
                crate::task_commands::candidate::performance::proof(&request, &repo, &task_state)?;
            print(&proof, &inspect)
        }
        ScenarioCommand::CandidateSource {
            file,
            repo,
            task_state,
            into,
            inspect,
        } => {
            let request: workspace::SourceRequest =
                serde_json::from_slice(&store::read(&file, MAX_BYTES)?)
                    .map_err(|e| e.to_string())?;
            request.validate()?;
            let receipt = crate::task_commands::candidate::candidate_source(
                &request,
                &repo,
                &task_state,
                &into,
            )?;
            print(&receipt, &inspect)
        }
        ScenarioCommand::ExperimentSource {
            file,
            repo,
            task_state,
            into,
            inspect,
        } => {
            let request: source::SourceRequest =
                serde_json::from_slice(&store::read(&file, MAX_BYTES)?)
                    .map_err(|e| e.to_string())?;
            request.validate()?;
            let receipt = crate::task_commands::candidate::experiment_source(
                &request,
                &repo,
                &task_state,
                &into,
            )?;
            if inspect.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", receipt.repository.display());
            }
            Ok(0)
        }
        ScenarioCommand::EvidenceSource {
            task_id,
            artifact_id,
            repo,
            task_state,
            inspect,
        } => print(
            &crate::task_commands::scenario_evidence::read(
                &repo,
                &task_state,
                &task_id,
                &artifact_id,
            )?,
            &inspect,
        ),
        ScenarioCommand::TemplatePin {
            file,
            repo,
            task_state,
            authority_commit,
            bindings,
            inspect,
        } => {
            let template = crate::task_commands::scenario_template::pin(
                repo,
                task_state,
                authority_commit,
                store::definition(&file)?,
                bindings.as_deref(),
            )?;
            let id = template.id()?;
            print(
                &serde_json::json!({"schema":"af/scenario-template-pin@2","template_id":id,"template":template}),
                &inspect,
            )
        }
        ScenarioCommand::Roadmap { command } => roadmap_command(command),
        ScenarioCommand::Validate {
            definition,
            inspect,
        } => print(&pin(&definition)?, &inspect),
        ScenarioCommand::Start {
            run_id,
            definition,
            inputs,
            confirm_pin,
            execute,
            inspect,
        } => {
            let pin = pin(&definition)?;
            require(
                pin.revision == confirm_pin,
                "Scenario pin confirmation differs; validate and review it again",
            )?;
            let limits = pin.bundle.definitions[&pin.bundle.root].limits.clone();
            let run = Run::new(
                &AfHost,
                run_id.clone(),
                pin,
                store::load(&inputs)?,
                limits,
                at(&inspect)?,
                None,
            )?;
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), true)?;
            runtime::reconcile_pending(&store, run.created_unix_ms)?;
            let run = store.update(|world| world.admit_run(run))?;
            let run = if execute {
                runtime::resume(&store, &run_id, inspect.now)?
            } else {
                run
            };
            print(&run, &inspect)
        }
        ScenarioCommand::Show { run_id, inspect } => {
            let world = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?.read()?;
            print(
                world.runs.get(&run_id).ok_or("Scenario Run not found")?,
                &inspect,
            )
        }
        ScenarioCommand::List { inspect } => {
            let world = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?.read()?;
            print(
                &serde_json::json!({"schema":"af/scenario-list@6","runs":world.runs}),
                &inspect,
            )
        }
        ScenarioCommand::Resume { run_id, inspect } => {
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            print(&runtime::resume(&store, &run_id, inspect.now)?, &inspect)
        }
        ScenarioCommand::Cancel {
            run_id,
            reason,
            inspect,
        } => {
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            print(
                &runtime::cancel(&store, &run_id, reason, at(&inspect)?)?,
                &inspect,
            )
        }
        ScenarioCommand::DecisionPayload {
            run_id,
            out,
            reject,
            inspect,
        } => {
            let world = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?.read()?;
            let run = world.runs.get(&run_id).ok_or("Scenario Run not found")?;
            let Some(Waiting::Decision { request, .. }) = &run.waiting else {
                return Err("Run is not waiting for a human decision".into());
            };
            let mut request = request.clone();
            request.approve = !reject;
            require(
                request.expires_unix_ms > at(&inspect)?,
                "Decision request expired",
            )?;
            use std::io::Write;
            let mut file = af_store::local_fs::private_file_options()
                .write(true)
                .create_new(true)
                .open(out)
                .map_err(|e| e.to_string())?;
            file.write_all(&request.signing_bytes()?)
                .and_then(|()| file.sync_all())
                .map_err(|e| e.to_string())?;
            print(&request, &inspect)
        }
        ScenarioCommand::Decide {
            run_id,
            file,
            inspect,
        } => {
            let decision: DecisionReceipt = store::load(&file)?;
            let at = at(&inspect)?;
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            let run = store.update(|world| {
                let run = world
                    .runs
                    .get_mut(&run_id)
                    .ok_or("Scenario Run not found")?;
                run.apply_decision(decision, at)?;
                Ok(run.clone())
            })?;
            print(&run, &inspect)
        }
    }
}
pub(crate) fn ant(command: AntCommand) -> Result<i32> {
    match command {
        AntCommand::Scheduler { command } => scheduler(command),
        AntCommand::Output { id, view, inspect } => output(&id, true, &view, &inspect),
        AntCommand::Validate {
            definition,
            inspect,
        } => print(&ant_pin(&definition)?, &inspect),
        AntCommand::Add {
            definition,
            confirm_pin,
            inspect,
        } => {
            let pin = ant_pin(&definition)?;
            require(
                pin.revision == confirm_pin,
                "Ant pin confirmation differs; validate and review it again",
            )?;
            let id = pin.definition.id.clone();
            let ant = AntState::new(pin, at(&inspect)?)?;
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), true)?;
            store.update(|world| {
                require(!world.ants.contains_key(&id), "Ant already exists")?;
                world.ants.insert(id, ant.clone());
                Ok(())
            })?;
            print(&ant, &inspect)
        }
        AntCommand::Show { ant_id, inspect } => {
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            let world = store.read()?;
            print(
                &ant_inspection(
                    world.ants.get(&ant_id).ok_or("Ant not found")?,
                    &world,
                    &store.scheduler_status()?,
                )?,
                &inspect,
            )
        }
        AntCommand::List { inspect } => {
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            let world = store.read()?;
            let scheduler = store.scheduler_status()?;
            let ants = world
                .ants
                .iter()
                .map(|(id, ant)| Ok((id.clone(), ant_inspection(ant, &world, &scheduler)?)))
                .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
            print(
                &serde_json::json!({"schema":"af/ant-list@8","ants":ants}),
                &inspect,
            )
        }
        AntCommand::Enable { ant_id, inspect } => change(&ant_id, AntMode::Enabled, &inspect),
        AntCommand::Pause { ant_id, inspect } => change(&ant_id, AntMode::Paused, &inspect),
        AntCommand::Disable { ant_id, inspect } => change(&ant_id, AntMode::Disabled, &inspect),
        AntCommand::Tick { inspect } => {
            let now = at(&inspect)?;
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            let runtime::Tick {
                started,
                refused,
                busy,
                runs: results,
            } = runtime::tick(&store, now, inspect.now)?;
            print(
                &serde_json::json!({"schema":"af/ant-tick@7","now_unix_ms":now,"started":started,"busy":busy,"refused":refused,"runs":results}),
                &inspect,
            )
        }
    }
}
fn ant_inspection(
    ant: &AntState,
    world: &store::World,
    scheduler: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(ant).map_err(|e| e.to_string())?;
    value["schema"] = serde_json::json!("af/ant-inspection@3");
    value["scheduler"] = scheduler.clone();
    let next_due = ant.next_due_unix_ms();
    value["next_due_unix_ms"] = serde_json::json!(next_due.as_ref().ok().copied().flatten());
    value["next_due_error"] = serde_json::json!(next_due.err());
    value["active_run"] =
        serde_json::json!(ant.occurrences.iter().rev().find_map(|o| match &o.outcome {
            OccurrenceOutcome::Started { run_id, .. }
                if world.runs.get(run_id).is_some_and(|r| r.status.active()) =>
                Some(run_id),
            _ => None,
        }));
    value["last_lateness_ms"] = serde_json::json!(
        ant.occurrences
            .last()
            .map(|o| o.admitted_unix_ms - o.due_unix_ms)
    );
    Ok(value)
}
fn change(id: &str, state: AntMode, inspect: &Inspect) -> Result<i32> {
    let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
    let now = at(inspect)?;
    let ant = store.update(|world| {
        let ant = world.ants.get_mut(id).ok_or("Ant not found")?;
        ant.change(state, now)?;
        Ok(ant.clone())
    })?;
    print(&ant, inspect)
}

#[derive(Debug, Subcommand)]
pub(crate) enum RoadmapCommand {
    /// Validate a proposal and print the exact revision to review; creates no state
    Validate {
        /// Versioned goal proposal (TOML authoring or JSON wire format)
        #[arg(long)]
        file: PathBuf,
        /// Existing roadmap label when planning a revision, rather than a new roadmap
        #[arg(long)]
        roadmap: Option<String>,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Create pending goals with zero actual charges after exact revision confirmation
    Create {
        /// Versioned goal proposal (TOML authoring or JSON wire format)
        #[arg(long)]
        file: PathBuf,
        /// Exact revision from validate; binds the proposal content and parent
        #[arg(long)]
        confirm_revision: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Append an immutable revision; preserve stable goal identities and runtime evidence
    #[command(
        override_usage = "af scenario roadmap revise [OPTIONS] --file <FILE>\n       --confirm-revision <CONFIRM_REVISION> <ROADMAP_ID>"
    )]
    Revise {
        /// Local roadmap label in the selected Scenario Store
        roadmap_id: String,
        /// Versioned goal proposal (TOML authoring or JSON wire format)
        #[arg(long)]
        file: PathBuf,
        /// Exact revision from validate; binds the proposal content and parent
        #[arg(long)]
        confirm_revision: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// Inspect complete revision history, proposed goals and runtime-owned progress
    Show {
        /// Local roadmap label in the selected Scenario Store
        roadmap_id: String,
        #[command(flatten)]
        inspect: Inspect,
    },
    /// List goal roadmaps in the selected private Scenario Store
    List {
        #[command(flatten)]
        inspect: Inspect,
    },
}
fn roadmap_command(command: RoadmapCommand) -> Result<i32> {
    use roadmap::{Proposal, Revision, Roadmap};
    match command {
        RoadmapCommand::Validate {
            file,
            roadmap,
            inspect,
        } => {
            let proposal: Proposal = store::definition(&file)?;
            let revision = if let Some(id) = roadmap {
                let world =
                    Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?.read()?;
                world
                    .roadmaps
                    .get(&id)
                    .ok_or("Roadmap not found")?
                    .plan(proposal)?
            } else {
                Revision::new(None, proposal)?
            };
            print(&revision, &inspect)
        }
        RoadmapCommand::Create {
            file,
            confirm_revision,
            inspect,
        } => {
            let revision = Revision::new(None, store::definition(&file)?)?;
            require(
                revision.revision == confirm_revision,
                "Roadmap revision confirmation differs; validate and review it again",
            )?;
            let roadmap = Roadmap::new(revision)?;
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), true)?;
            store.update(|world| {
                require(
                    !world.roadmaps.contains_key(&roadmap.id),
                    "Roadmap already exists; inspect or revise it",
                )?;
                world.roadmaps.insert(roadmap.id.clone(), roadmap.clone());
                Ok(())
            })?;
            print(&roadmap, &inspect)
        }
        RoadmapCommand::Revise {
            roadmap_id,
            file,
            confirm_revision,
            inspect,
        } => {
            let proposal: Proposal = store::definition(&file)?;
            let store = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?;
            let roadmap = store.update(|world| {
                let roadmap = world
                    .roadmaps
                    .get_mut(&roadmap_id)
                    .ok_or("Roadmap not found")?;
                roadmap.revise(proposal, &confirm_revision)?;
                Ok(roadmap.clone())
            })?;
            print(&roadmap, &inspect)
        }
        RoadmapCommand::Show {
            roadmap_id,
            inspect,
        } => {
            let world = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?.read()?;
            print(
                world.roadmaps.get(&roadmap_id).ok_or("Roadmap not found")?,
                &inspect,
            )
        }
        RoadmapCommand::List { inspect } => {
            let world = Store::open(Arc::new(AfHost), inspect.state.as_deref(), false)?.read()?;
            print(
                &serde_json::json!({"schema":"af/scenario-roadmap-list@3","roadmaps":world.roadmaps}),
                &inspect,
            )
        }
    }
}
