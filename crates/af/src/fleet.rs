//! Machine-local clients for read-only infrastructure and Project discovery.

pub(crate) mod cache;
mod context;
mod inventory;
pub(crate) mod origin;
mod transport;

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use af_contracts::fleet::{
    Action, Allocation, BriefingV2, ClusterState, FLEET_BRIEFING_V2, Freshness, Inventory,
    NextAction, Observation, Readiness, Reading, View,
};
use clap::{Args, Subcommand};
use serde::{Serialize, de::DeserializeOwned};

use cache::Cache;
use context::{Binding, Config, Descriptor};
use transport::{ALLOCATIONS, POOLS, ReadFailure, Target};

const COMMAND_WALL: Duration = Duration::from_secs(20);
const CONNECT_WALL: Duration = Duration::from_secs(30);

type PoolsRead = Result<Inventory, ReadFailure>;
type AllocationsRead = Result<Vec<Allocation>, ReadFailure>;

struct ManagementReader<'a> {
    target: &'a Target,
    key: PathBuf,
    deadline: Instant,
}

impl ManagementReader<'_> {
    fn pools(&self) -> PoolsRead {
        self.target
            .read(&self.key, POOLS, self.deadline)
            .and_then(inventory::Pools::normalize)
    }

    fn allocations(&self) -> AllocationsRead {
        self.target
            .read(&self.key, ALLOCATIONS, self.deadline)
            .and_then(inventory::allocations)
    }

    fn both(&self) -> Result<(PoolsRead, AllocationsRead), &'static str> {
        std::thread::scope(|threads| {
            let pools = threads.spawn(|| self.pools());
            let allocations = self.allocations();
            let pools = pools
                .join()
                .map_err(|_| "Management pools reader terminated")?;
            Ok((pools, allocations))
        })
    }
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct InfraArgs {
    #[command(subcommand)]
    command: Option<InfraCommand>,
    #[command(flatten)]
    read: ReadArgs,
}

#[derive(Debug, Subcommand)]
enum InfraCommand {
    /// Authenticate through the pinned management transport and save a local context
    Connect {
        /// Local context name
        #[arg(long)]
        context: String,
        /// Owner-provided JSON connection descriptor
        #[arg(long, value_name = "FILE")]
        from: PathBuf,
        /// Local SSH identity reference under the fleet credentials directory
        #[arg(long)]
        credential_ref: String,
    },
    /// Read host headroom and pool availability
    Status(ReadArgs),
    /// Read provider allocation placement and power
    Allocations(ReadArgs),
}

#[derive(Debug, Args)]
struct ReadArgs {
    /// Select a local context (automatically selected when only one is configured)
    #[arg(long)]
    context: Option<String>,
    /// Print the versioned briefing
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct ClusterArgs {
    #[command(subcommand)]
    command: Option<ClusterCommand>,
    /// Select the infrastructure context for Project discovery
    #[arg(long)]
    infra: Option<String>,
    /// Print the versioned briefing
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ClusterCommand {
    /// Read waiting Tasks and Decisions from independently authorized Project origins
    Inbox(ReadArgs),
    /// Serve a single owner-authorized registered Workspace over a forced SSH command
    #[command(hide = true)]
    OriginRead(origin::ReaderArgs),
}

pub(crate) fn infra(args: InfraArgs) -> Result<i32, String> {
    let config = Config::open()?;
    let (view, read) = match args.command {
        Some(InfraCommand::Connect {
            context,
            from,
            credential_ref,
        }) => {
            return connect(config, context, from, credential_ref);
        }
        Some(InfraCommand::Status(read)) => (View::Status, read),
        Some(InfraCommand::Allocations(read)) => (View::Allocations, read),
        None => (View::Infra, args.read),
    };
    brief(config, view, read)
}

pub(crate) fn cluster(args: ClusterArgs) -> Result<i32, String> {
    let (view, read) = match args.command {
        Some(ClusterCommand::OriginRead(args)) => return origin::serve(args),
        Some(ClusterCommand::Inbox(read)) => (View::Inbox, read),
        None => (
            View::Cluster,
            ReadArgs {
                context: args.infra,
                json: args.json,
            },
        ),
    };
    brief(Config::open()?, view, read)
}

fn connect(
    config: Config,
    context: String,
    from: PathBuf,
    credential_ref: String,
) -> Result<i32, String> {
    validate_label(&context)?;
    validate_label(&credential_ref)?;
    let binding = Binding::new(Descriptor::load(&from)?, credential_ref);
    let previous = config.binding(&context)?;
    if previous.as_ref().is_some_and(|current| current != &binding) {
        return Err(context::conflict(&context));
    }

    let (pools, allocations) = ManagementReader {
        target: &binding.descriptor.management,
        key: config.management_key(&binding.credential_ref),
        deadline: Instant::now() + CONNECT_WALL,
    }
    .both()?;
    pools.map_err(|error| format!("Management pools capability: {}", error.reason()))?;
    allocations
        .map_err(|error| format!("Management allocations capability: {}", error.reason()))?;
    if previous.is_none() {
        config.publish(&context, &binding)?;
    }
    let mut briefing = initial(View::Connect, Some(context), config.aliases()?);
    briefing.scope = Some(binding.descriptor.scope);
    briefing.discovery = Readiness::Ready;
    briefing.cluster = binding.descriptor.cluster;
    briefing.next_action = next(
        Action::Run,
        "The pinned management reader is authenticated",
        "Read host headroom with af infra status",
    );
    print(&briefing, false)
}

fn initial(view: View, context: Option<String>, configured_contexts: Vec<String>) -> BriefingV2 {
    BriefingV2 {
        schema: FLEET_BRIEFING_V2.into(),
        view,
        context,
        scope: None,
        configured_contexts,
        discovery: Readiness::Unconfigured,
        cluster: ClusterState::Unknown,
        distributed_execution_ready: false,
        desired_footprint: None,
        inventory: None,
        allocations: None,
        origins: Vec::new(),
        next_action: next(
            Action::AskOwner,
            "No context is configured. Obtain an owner-provided descriptor and a local credential reference",
            "Connect with af infra connect",
        ),
    }
}

fn brief(config: Config, view: View, read: ReadArgs) -> Result<i32, String> {
    let aliases = config.aliases()?;
    if let Some(alias) = &read.context {
        validate_label(alias)?;
    }
    let selected = read
        .context
        .or_else(|| (aliases.len() == 1).then(|| aliases[0].clone()));
    let mut briefing = initial(view, selected.clone(), aliases);
    let Some(alias) = selected else {
        if !briefing.configured_contexts.is_empty() {
            briefing.next_action = next(
                Action::Run,
                "More than one context is configured",
                "Select a context explicitly",
            );
        }
        return print(&briefing, read.json);
    };
    let Some(binding) = config.binding(&alias)? else {
        briefing.next_action.reason = format!(
            "Context {alias} is missing. Obtain an owner-provided descriptor and a local credential reference"
        );
        return print(&briefing, read.json);
    };

    let descriptor = &binding.descriptor;
    briefing.scope = Some(descriptor.scope.clone());
    briefing.cluster = descriptor.cluster;
    let deadline = Instant::now() + COMMAND_WALL;
    let mut readings = Vec::new();
    let mut timed_out = false;
    if matches!(view, View::Infra | View::Status | View::Allocations) {
        let cache = Cache::new(&alias, &binding);
        let reader = ManagementReader {
            target: &descriptor.management,
            key: config.management_key(&binding.credential_ref),
            deadline,
        };
        let (pools, allocations) = match view {
            View::Infra => {
                let (pools, allocations) = reader.both()?;
                (Some(pools), Some(allocations))
            }
            View::Status => (Some(reader.pools()), None),
            View::Allocations => (None, Some(reader.allocations())),
            _ => unreachable!(),
        };
        if let Some(result) = pools {
            timed_out |= matches!(result, Err(ReadFailure::TimedOut));
            let reading = observe(&descriptor.scope, "pools", cache.as_ref(), result)?;
            readings.push(reading.observation.freshness);
            briefing.inventory = Some(reading);
        }
        if let Some(result) = allocations {
            timed_out |= matches!(result, Err(ReadFailure::TimedOut));
            let reading = observe(&descriptor.scope, "allocations", cache.as_ref(), result)?;
            readings.push(reading.observation.freshness);
            briefing.allocations = Some(reading);
        }
    } else {
        briefing.origins = std::thread::scope(|threads| {
            let readers: Vec<_> = descriptor
                .origins
                .iter()
                .map(|(name, reader)| {
                    let config = &config;
                    let alias = &alias;
                    threads.spawn(move || {
                        origin::read(config, alias, &descriptor.scope, name, reader, deadline)
                    })
                })
                .collect();
            readers
                .into_iter()
                .map(|reader| {
                    reader
                        .join()
                        .map_err(|_| "Project observation reader terminated")
                })
                .collect::<Result<Vec<_>, _>>()
        })?;
        readings.extend(
            briefing
                .origins
                .iter()
                .map(|reading| reading.observation.freshness),
        );
    }
    (briefing.discovery, briefing.next_action) = readiness(&readings, view);
    if timed_out && matches!(briefing.next_action.action, Action::Wait) {
        briefing.next_action.reason = "A management reader exceeded the command deadline".into();
    }
    print(&briefing, read.json)
}

fn readiness(readings: &[Freshness], view: View) -> (Readiness, NextAction) {
    if readings.is_empty() {
        return (
            Readiness::Unconfigured,
            next(
                Action::AskOwner,
                "No Project origins were supplied for this context",
                "Obtain an authorized Project reader from its owner",
            ),
        );
    }
    if readings.iter().all(|state| *state == Freshness::Fresh) {
        let action = match view {
            View::Cluster => next(
                Action::Run,
                "Project discovery is ready",
                "Read waiting Tasks with af cluster inbox",
            ),
            View::Inbox => next(
                Action::Run,
                "Authorized Project observations are current",
                "Review any waiting Tasks and their available Decisions",
            ),
            _ => next(
                Action::Run,
                "Infrastructure observations are current",
                "Use the reported headroom and allocation placement",
            ),
        };
        return (Readiness::Ready, action);
    }
    let readiness = if readings
        .iter()
        .any(|state| matches!(state, Freshness::Fresh | Freshness::Stale))
    {
        Readiness::Partial
    } else {
        Readiness::Unavailable
    };
    let action = if readings.iter().any(|state| {
        matches!(
            state,
            Freshness::IdentityMismatch | Freshness::InvalidResponse
        )
    }) {
        next(
            Action::Repair,
            "A reader failed identity or response validation",
            "Verify the owner-provided reader identity and protocol",
        )
    } else if readings
        .iter()
        .any(|state| matches!(state, Freshness::Denied | Freshness::MissingCredential))
    {
        next(
            Action::AskOwner,
            "A reader denied access or needs a local credential reference",
            "Obtain the required read grant and configure its SSH identity",
        )
    } else {
        next(
            Action::Wait,
            "A reader is unavailable or stale",
            "Retry when all selected readers can return a current observation",
        )
    };
    (readiness, action)
}

fn observe<T: Serialize + DeserializeOwned>(
    scope: &str,
    subject: &str,
    cache: Option<&Cache>,
    result: Result<T, ReadFailure>,
) -> Result<Reading<T>, String> {
    let now = now_ms()?;
    Ok(match result {
        Ok(data) => {
            if let Some(cache) = cache {
                cache.remember(subject, now, &data);
            }
            Reading {
                observation: observation(scope, subject, Some((now, 0)), Freshness::Fresh),
                data: Some(data),
            }
        }
        Err(ReadFailure::Temporary) => match cache.and_then(|cache| cache.last(subject, now)) {
            Some(record) => Reading {
                observation: observation(
                    &record.source,
                    subject,
                    Some((record.observed_unix_ms, now - record.observed_unix_ms)),
                    Freshness::Stale,
                ),
                data: Some(record.data),
            },
            None => unavailable(scope, subject, ReadFailure::Temporary),
        },
        Err(failure) => unavailable(scope, subject, failure),
    })
}

fn observation(
    source: &str,
    subject: &str,
    time: Option<(u64, u64)>,
    freshness: Freshness,
) -> Observation {
    Observation {
        source: source.into(),
        subject: subject.into(),
        revision: None,
        observed_unix_ms: time.map(|(at, _)| at),
        age_ms: time.map(|(_, age)| age),
        freshness,
    }
}

fn unavailable<T>(source: &str, subject: &str, error: ReadFailure) -> Reading<T> {
    Reading {
        observation: observation(source, subject, None, error.freshness()),
        data: None,
    }
}

fn next(action: Action, reason: &str, success_condition: &str) -> NextAction {
    NextAction {
        action,
        reason: reason.into(),
        success_condition: success_condition.into(),
    }
}

fn now_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_millis() as u64)
        .map_err(|error| error.to_string())
}

pub(crate) fn is_label(label: &str) -> bool {
    label.len() <= 64
        && label
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn validate_label(label: &str) -> Result<(), String> {
    if is_label(label) {
        Ok(())
    } else {
        Err("Fleet names must start with an ASCII letter or digit and contain at most 64 letters, digits, hyphens or underscores".into())
    }
}

fn print(briefing: &BriefingV2, json: bool) -> Result<i32, String> {
    if json {
        println!(
            "{}",
            serde_json::to_string(briefing).map_err(|e| e.to_string())?
        );
    } else {
        render(briefing);
    }
    Ok(0)
}

fn word(value: impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => unreachable!("fleet text labels serialize as strings"),
    }
}

fn describe(observation: &Observation) {
    let age = observation
        .age_ms
        .map_or_else(|| "unknown age".into(), |age| format!("{age} ms old"));
    println!(
        "  {} / {} · {} · {age}",
        crate::terminal_text(&observation.source),
        crate::terminal_text(&observation.subject),
        word(observation.freshness)
    );
}

fn render(briefing: &BriefingV2) {
    let context = briefing.context.as_deref().unwrap_or("no context");
    println!("{context} · discovery {}", word(briefing.discovery));
    if let Some(scope) = &briefing.scope {
        println!("Scope {scope}");
    }
    if briefing.context.is_none() && !briefing.configured_contexts.is_empty() {
        println!(
            "Configured contexts: {}",
            briefing.configured_contexts.join(", ")
        );
    }
    if matches!(briefing.view, View::Cluster | View::Inbox) {
        println!(
            "Cluster {} · distributed execution unavailable",
            word(briefing.cluster)
        );
    }
    if matches!(briefing.view, View::Infra | View::Status) {
        println!("Desired footprint unknown");
    }
    if let Some(reading) = &briefing.inventory {
        describe(&reading.observation);
        if let Some(inventory) = &reading.data {
            for host in &inventory.hosts {
                println!(
                    "    {} · {} allocatable vCPUs · {} GiB RAM · {} GiB SSD",
                    crate::terminal_text(&host.node),
                    host.available.cpu_ids.len(),
                    host.available.ram_gib,
                    host.available.ssd_gib
                );
                if let Some(capacity) = &host.static_available {
                    println!(
                        "      Static availability · {} vCPUs · {} GiB RAM · {} GiB SSD",
                        capacity.cpu_ids.len(),
                        capacity.ram_gib,
                        capacity.ssd_gib
                    );
                }
            }
            match inventory.pools.as_deref() {
                None => println!("    Storage pool inventory unknown"),
                Some([]) => println!("    No storage pools"),
                Some(pools) => {
                    for pool in pools {
                        let classes = match (pool.static_pool, pool.dynamic_pool) {
                            (true, true) => "static and dynamic",
                            (true, false) => "static",
                            _ => "dynamic",
                        };
                        println!(
                            "    Pool {} ({classes}) · capacity unknown",
                            crate::terminal_text(&pool.name)
                        );
                    }
                }
            }
            println!(
                "    Reservations and physical totals unknown · release {}",
                word(&inventory.release)
            );
        }
    }
    if let Some(reading) = &briefing.allocations {
        describe(&reading.observation);
        if let Some(allocations) = &reading.data {
            for allocation in allocations {
                let field = |value: &Option<String>| {
                    crate::terminal_text(value.as_deref().unwrap_or("unknown"))
                };
                let charges = allocation.charged.as_ref().map_or_else(
                    || "charges unknown".to_string(),
                    |charged| {
                        format!(
                            "charged {} vCPUs · {} GiB RAM · {} GB disk",
                            charged.cpu_ids.len(),
                            charged.ram_gib,
                            charged.ssd_gib * 1_073_741_824.0 / 1_000_000_000.0
                        )
                    },
                );
                println!(
                    "    VM {} · {} · host {} · power {} · state {} · {charges} · executor {}",
                    allocation.vmid,
                    field(&allocation.name),
                    field(&allocation.node),
                    field(&allocation.power),
                    field(&allocation.state),
                    field(&allocation.executor)
                );
            }
        }
    }
    for reading in &briefing.origins {
        describe(&reading.observation);
        if let Some(origin) = &reading.data {
            println!(
                "    Project {} · Store {} · principal {}",
                origin.project_id, origin.store_id, origin.principal
            );
            if origin.tasks.is_empty() {
                println!("    No waiting Tasks");
            }
            for task in &origin.tasks {
                println!(
                    "    {} · {} · {} · host {}",
                    task.task_id,
                    word(&task.phase),
                    word(task.reason),
                    crate::terminal_text(task.placement.as_deref().unwrap_or("unknown"))
                );
                describe(&task.observation);
                if let Some(decision) = &task.decision {
                    println!(
                        "      {} may approve or reject plan {} for revision {}",
                        decision.developer, decision.plan_id, decision.task_revision_id
                    );
                    if let Some(prepared) = &decision.prepared_id {
                        println!("        Prepared experiment {prepared}");
                    }
                }
            }
        }
    }
    println!(
        "Next {} · {}. {}.",
        word(briefing.next_action.action),
        briefing.next_action.reason,
        briefing.next_action.success_condition
    );
}
