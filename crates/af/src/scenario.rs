//! Scenarios orchestrate Tasks and bounded effects; Ants admit durable daily occurrences.
mod adapter;
mod contract;
mod runtime;
mod schedule;
mod state;
mod store;
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
    /// Owner-only machine policy outside the checkout; grants exact Task plans and adapters
    #[arg(long)]
    authority: PathBuf,
}
#[derive(Debug, Subcommand)]
pub(crate) enum ScenarioCommand {
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
            store.update(|world| {
                require(
                    !world.runs.contains_key(&run_id),
                    "Run ID already exists; inspect or resume it",
                )?;
                world.runs.insert(run_id.clone(), run.clone());
                Ok(())
            })?;
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
                &serde_json::json!({"schema":"af/scenario-list@1","runs":world.runs}),
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
                &serde_json::json!({"schema":"af/ant-list@1","ants":world.ants}),
                &inspect,
            )
        }
        AntCommand::Enable { ant_id, inspect } => change(&ant_id, AntMode::Enabled, &inspect),
        AntCommand::Pause { ant_id, inspect } => change(&ant_id, AntMode::Paused, &inspect),
        AntCommand::Disable { ant_id, inspect } => change(&ant_id, AntMode::Disabled, &inspect),
        AntCommand::Tick { inspect } => {
            let now = at(&inspect)?;
            let store = Store::open(inspect.state.as_deref(), false)?;
            let (started, refused) = store
                .update(|world| Ok(schedule::tick_all(&mut world.ants, &mut world.runs, now)))?;
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
                &serde_json::json!({"schema":"af/ant-tick@1","now_unix_ms":now,"started":started,"busy":busy,"refused":refused,"runs":results}),
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
