//! Scenarios orchestrate Tasks and bounded effects; Ants admit durable daily occurrences.
mod adapter;
#[cfg(test)]
mod admission_tests;
pub(crate) mod contract;
pub(crate) mod implementation;
mod roadmap;
mod runtime;
mod schedule;
pub(crate) mod source;
mod state;
mod store;
pub(crate) mod template;
#[cfg(test)]
mod tests;

use clap::{Args, Subcommand};
use contract::*;
use schedule::*;
use state::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
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
    /// Capture one accepted implementation candidate as private, filter-free Git objects
    #[command(
        override_usage = "af scenario impl-source --file FILE --repo REPO\n                        --task-state STORE --into DIR [OPTIONS]",
        long_about = "Validate an exact accepted Task receipt and its committed source, then encode its candidate Snapshot as private Git objects. No source checkout, branch or remote is changed. Existing complete captures are verified and reused; incomplete or foreign captures are refused.",
        after_long_help = "Examples:\n  # needs: an exact accepted implementation Task receipt and committed source\n  af scenario impl-source --file source.json --repo /project \\\n    --task-state /private/tasks --into /private/staging/source --json"
    )]
    ImplSource {
        /// af/scenario-impl-source-request@1 request naming the accepted Task and source commit
        #[arg(long)]
        file: PathBuf,
        /// Repository the accepted Task belongs to
        #[arg(long)]
        repo: PathBuf,
        /// Private native Task Store
        #[arg(long)]
        task_state: PathBuf,
        /// Private capture leaf outside the repository and Task Store
        #[arg(long)]
        into: PathBuf,
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
        /// Native slots and Provider bindings; local package paths are refused
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
fn pin(definition: &Definition) -> Result<Pin> {
    let mut bundle: Bundle = store::definition(&definition.file)?;
    normalize_targets(&mut bundle)?;
    Pin::new(bundle, store::policy(&definition.authority)?)
}
fn ant_pin(definition: &Definition) -> Result<AntPin> {
    let mut ant: Ant = store::definition(&definition.file)?;
    normalize_targets(&mut ant.scenario)?;
    AntPin::new(ant, store::policy(&definition.authority)?)
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
        ScenarioCommand::ImplSource {
            file,
            repo,
            task_state,
            into,
            inspect,
        } => {
            let request: implementation::SourceRequest =
                serde_json::from_slice(&store::read(&file, MAX_BYTES)?)
                    .map_err(|e| e.to_string())?;
            let receipt = crate::task_execution::candidate::implementation_source(
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
            let receipt = crate::task_execution::candidate::experiment_source(
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
            &crate::task_execution::scenario_evidence::read(
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
            let template = crate::task_execution::scenario_template::pin(
                repo,
                task_state,
                authority_commit,
                store::definition(&file)?,
                bindings
                    .as_ref()
                    .map(|p| store::definition(p))
                    .transpose()?,
            )?;
            let id = template.id()?;
            print(
                &serde_json::json!({"schema":"af/scenario-template-pin@1","template_id":id,"template":template}),
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
                run_id.clone(),
                pin,
                store::load(&inputs)?,
                limits,
                at(&inspect)?,
                None,
            )?;
            let store = Store::open(inspect.state.as_deref(), true)?;
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
            let world = Store::open(inspect.state.as_deref(), false)?.read()?;
            print(
                world.runs.get(&run_id).ok_or("Scenario Run not found")?,
                &inspect,
            )
        }
        ScenarioCommand::List { inspect } => {
            let world = Store::open(inspect.state.as_deref(), false)?.read()?;
            print(
                &serde_json::json!({"schema":"af/scenario-list@4","runs":world.runs}),
                &inspect,
            )
        }
        ScenarioCommand::Resume { run_id, inspect } => {
            let store = Store::open(inspect.state.as_deref(), false)?;
            print(&runtime::resume(&store, &run_id, inspect.now)?, &inspect)
        }
        ScenarioCommand::Cancel {
            run_id,
            reason,
            inspect,
        } => {
            let store = Store::open(inspect.state.as_deref(), false)?;
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
            let world = Store::open(inspect.state.as_deref(), false)?.read()?;
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
            let mut file = review_store::local_fs::private_file_options()
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
            let store = Store::open(inspect.state.as_deref(), false)?;
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
            let store = Store::open(inspect.state.as_deref(), true)?;
            store.update(|world| {
                require(!world.ants.contains_key(&id), "Ant already exists")?;
                world.ants.insert(id, ant.clone());
                Ok(())
            })?;
            print(&ant, &inspect)
        }
        AntCommand::Show { ant_id, inspect } => {
            let world = Store::open(inspect.state.as_deref(), false)?.read()?;
            print(world.ants.get(&ant_id).ok_or("Ant not found")?, &inspect)
        }
        AntCommand::List { inspect } => {
            let world = Store::open(inspect.state.as_deref(), false)?.read()?;
            print(
                &serde_json::json!({"schema":"af/ant-list@4","ants":world.ants}),
                &inspect,
            )
        }
        AntCommand::Enable { ant_id, inspect } => change(&ant_id, AntMode::Enabled, &inspect),
        AntCommand::Pause { ant_id, inspect } => change(&ant_id, AntMode::Paused, &inspect),
        AntCommand::Disable { ant_id, inspect } => change(&ant_id, AntMode::Disabled, &inspect),
        AntCommand::Tick { inspect } => {
            let now = at(&inspect)?;
            let store = Store::open(inspect.state.as_deref(), false)?;
            runtime::reconcile_pending(&store, now)?;
            let (started, refused) = store.update(|world| {
                Ok(schedule::tick_all_with_roadmaps(
                    &mut world.ants,
                    &mut world.runs,
                    &mut world.roadmaps,
                    now,
                ))
            })?;
            // Admission and execution are separate durable transitions. Recover an occurrence
            // admitted by a process that died before driving it, and poll existing waits too.
            let active: Vec<String> = store
                .read()?
                .runs
                .values()
                .filter(|r| r.ant.is_some() && r.status.active() && r.updated_unix_ms <= now)
                .map(|r| r.id.clone())
                .collect();
            let mut results = BTreeMap::new();
            let mut busy = Vec::new();
            for id in &active {
                match runtime::resume(&store, id, inspect.now) {
                    Ok(run) => {
                        results.insert(id.clone(), run);
                    }
                    Err(why) if why.starts_with("Scenario Run is already executing:") => {
                        busy.push(id.clone())
                    }
                    Err(why) => return Err(why),
                }
            }
            print(
                &serde_json::json!({"schema":"af/ant-tick@4","now_unix_ms":now,"started":started,"busy":busy,"refused":refused,"runs":results}),
                &inspect,
            )
        }
    }
}
fn change(id: &str, state: AntMode, inspect: &Inspect) -> Result<i32> {
    let store = Store::open(inspect.state.as_deref(), false)?;
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
                let world = Store::open(inspect.state.as_deref(), false)?.read()?;
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
            let store = Store::open(inspect.state.as_deref(), true)?;
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
            let store = Store::open(inspect.state.as_deref(), false)?;
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
            let world = Store::open(inspect.state.as_deref(), false)?.read()?;
            print(
                world.roadmaps.get(&roadmap_id).ok_or("Roadmap not found")?,
                &inspect,
            )
        }
        RoadmapCommand::List { inspect } => {
            let world = Store::open(inspect.state.as_deref(), false)?.read()?;
            print(
                &serde_json::json!({"schema":"af/scenario-roadmap-list@2","roadmaps":world.roadmaps}),
                &inspect,
            )
        }
    }
}
