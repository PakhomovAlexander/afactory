//! Receiver-owned enrollment for the Project journal's daily backlog occurrences.
use super::*;
use crate::task_commands::scenario_portable as portable;
use af_contracts::backlog::batch::BatchReference;
use af_project::ProjectStore;
use af_project::backlog::batch::{ENROLLMENT_FILE, ScheduleScope};
use af_scenario::template::Template;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const SCHEMA: &str = "af/backlog-schedule@1";
const RUN_PREFIX: &str = "backlog-daily-";

#[derive(Debug, Args)]
pub(crate) struct Receiver {
    /// Receiver's registered checkout
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Committed repository-relative backlog Scenario artifact
    #[arg(long)]
    file: String,
    /// Private native Task Store outside the checkout
    #[arg(long)]
    task_state: PathBuf,
    /// Optional receiver-owned af.task-bindings/1 Provider aliases
    #[arg(long)]
    bindings: Option<PathBuf>,
    /// Accountable owner of this enrollment
    #[arg(long)]
    owner: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Capture and show the exact receiver scope for review, without admitting a batch
    Validate {
        #[command(flatten)]
        receiver: Receiver,
    },
    /// Enroll the reviewed scope for this Project
    #[command(
        override_usage = "af ant backlog enroll --file <FILE> --task-state <TASK_STATE>\n                      --owner <OWNER> --confirm-pin <REVISION> [OPTIONS]"
    )]
    Enroll {
        #[command(flatten)]
        receiver: Receiver,
        /// Exact revision printed by validate for the reviewed receiver scope
        #[arg(long, value_name = "REVISION")]
        confirm_pin: String,
    },
    /// Inspect the local enrollment and its pause state
    Show {
        /// Receiver's registered checkout
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Pause future batch admission while outstanding Runs can still settle
    Pause {
        /// Receiver's registered checkout
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Record due paused slots and enable future daily occurrences
    Resume {
        /// Receiver's registered checkout
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePin {
    path: PathBuf,
    digest: String,
}

impl FilePin {
    fn capture(path: &Path) -> Result<Self> {
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        let digest = digest(&store::read(&path, 1024 * 1024)?);
        Ok(Self { path, digest })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    owner: String,
    project_id: String,
    scenario_state: PathBuf,
    repo: PathBuf,
    commit: String,
    file: String,
    task_state: PathBuf,
    template_scope: String,
    bindings: Option<FilePin>,
    providers: FilePin,
    scenario_id: String,
    intake_authority: String,
    batch_policy: String,
    intake_policy: String,
    destination: String,
    limits: Limits,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    schema: String,
    revision: String,
    scope: Scope,
    paused: bool,
}

impl Scope {
    fn validate_project(&self, project: &ScheduleScope) -> Result<()> {
        require(
            self.project_id == project.policy.project_id
                && self.scenario_id == project.policy.scenario_id
                && self.intake_authority == project.policy.authority_id
                && self.batch_policy == project.policy_pin
                && self.intake_policy == project.intake_policy
                && self.destination == project.destination,
            "Recurring backlog scope differs from the installed Project authority",
        )
    }

    fn validate_receiver(&self) -> Result<()> {
        let checkout = af_project::require_checkout(&self.repo)?;
        let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
        let commit = af_source_git::git::Repo::open(&self.repo, temporary.path())
            .rev_parse("HEAD")
            .map_err(|e| e.to_string())?;
        require(
            checkout.toplevel == self.repo
                && checkout.project_id == self.project_id
                && commit == self.commit
                && providers()? == self.providers
                && self
                    .bindings
                    .as_ref()
                    .map(|pin| FilePin::capture(&pin.path))
                    .transpose()?
                    == self.bindings,
            "Recurring backlog checkout, Provider registry or bindings changed",
        )
    }

    fn validate_store(&self, requested: Option<&Path>) -> Result<()> {
        if let Some(requested) = requested {
            require(
                crate::resolve_filesystem_path(requested)? == self.scenario_state,
                "Recurring backlog enrollment uses a different Scenario Store",
            )?;
        }
        Ok(())
    }

    fn bindings_path(&self) -> Option<&Path> {
        self.bindings.as_ref().map(|pin| pin.path.as_path())
    }

    fn validate_bundle(&self, bundle: &Bundle) -> Result<BatchReference> {
        let root = &bundle.definitions[&bundle.root];
        let template = root
            .steps
            .iter()
            .find_map(|step| match &step.operation {
                Kind::FreshTask { template, .. } => Some(template.as_ref()),
                _ => None,
            })
            .ok_or("Recurring backlog Run lacks its native Task template")?;
        require(
            portable::backlog_template_scope(template)? == self.template_scope
                && root.limits == self.limits
                && root.authority.grants
                    == BTreeSet::from([
                        Grant {
                            adapter: "builtin".into(),
                            operation: Operation::BacklogSettleV1,
                            destination: self.destination.clone(),
                        },
                        Grant {
                            adapter: "builtin".into(),
                            operation: Operation::BacklogDispatchV1,
                            destination: self.destination.clone(),
                        },
                    ]),
            "Recurring backlog bundle differs from the reviewed template, limits or destination",
        )?;
        let reference = portable::backlog_template_reference(template)?;
        reference.validate()?;
        require(
            reference.project_id == self.project_id && reference.policy_id == self.batch_policy,
            "Recurring backlog bundle belongs to a different Project or batch policy",
        )?;
        Ok(reference)
    }
}

fn providers() -> Result<FilePin> {
    FilePin::capture(
        &crate::providers::registry_location()
            .ok_or("Recurring backlog requires a local Provider registry")?,
    )
}

fn review(receiver: Receiver, inspect: &Inspect, project_id: &str) -> Result<Enrollment> {
    let state = crate::resolve_filesystem_path(
        &inspect
            .state
            .clone()
            .map_or_else(|| AfHost::default_store_path(&AfHost), Ok)?,
    )?;
    let checkout = af_project::require_checkout(&receiver.repo)?;
    require(
        checkout.project_id == project_id,
        "Recurring backlog receiver Project changed",
    )?;
    name(&receiver.owner)?;
    let project = read_project(project_id)?.backlog_schedule_scope()?;
    let bindings = receiver
        .bindings
        .as_deref()
        .map(FilePin::capture)
        .transpose()?;
    let providers = providers()?;
    let (template, limits) = portable::review_backlog(
        checkout.toplevel,
        receiver.task_state,
        &receiver.file,
        project.policy.scenario_id.clone(),
        bindings.as_ref().map(|pin| pin.path.as_path()),
    )?;
    let policy = &project.policy;
    require(
        limits.within(&Limits {
            max_tokens: policy.run_tokens,
            max_wall_ms: policy.run_wall_ms,
            max_dispatches: policy.run_dispatches,
        }),
        "Recurring backlog Scenario exceeds the Project batch policy",
    )?;
    let scope = Scope {
        owner: receiver.owner,
        project_id: project_id.to_owned(),
        scenario_state: state,
        repo: template.repo.clone(),
        commit: template.authority_commit.clone(),
        file: receiver.file,
        task_state: template.state.clone(),
        template_scope: portable::backlog_template_scope(&template)?,
        bindings,
        providers,
        scenario_id: project.policy.scenario_id,
        intake_authority: project.policy.authority_id,
        batch_policy: project.policy_pin,
        intake_policy: project.intake_policy,
        destination: project.destination,
        limits,
    };
    scope.validate_receiver()?;
    Ok(Enrollment {
        schema: SCHEMA.into(),
        revision: hash(&(SCHEMA, &scope))?,
        scope,
        paused: false,
    })
}

fn load(directory: &Path) -> Result<Option<Enrollment>> {
    let path = directory.join(ENROLLMENT_FILE);
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    require(
        meta.is_file() && meta.uid() == nix::unistd::geteuid().as_raw() && meta.mode() & 0o077 == 0,
        "Recurring backlog enrollment must be an owner-only regular file",
    )?;
    let enrollment: Enrollment = serde_json::from_slice(&store::read(&path, 16_384)?)
        .map_err(|_| "Invalid recurring backlog enrollment")?;
    require(
        enrollment.schema == SCHEMA
            && enrollment.revision == hash(&(SCHEMA, &enrollment.scope))?
            && directory.file_name().and_then(|name| name.to_str())
                == enrollment.scope.project_id.strip_prefix("sha256:"),
        "Recurring backlog enrollment identity differs",
    )?;
    Ok(Some(enrollment))
}

fn save(project: &ProjectStore, enrollment: &Enrollment) -> Result<()> {
    af_scenario::presentation::atomic_write(
        &project.directory.join(ENROLLMENT_FILE),
        &serde_json::to_string(enrollment).map_err(|e| e.to_string())?,
    )
}

fn read_project(id: &str) -> Result<ProjectStore> {
    require(
        af_contracts::is_digest(id),
        "Invalid recurring backlog Project ID",
    )?;
    ProjectStore::open_read_only(&crate::xdg_state_root()?, id)
}

fn write_project(id: &str) -> Result<ProjectStore> {
    ProjectStore::open_waiting(
        &crate::project::Application,
        &crate::xdg_state_root()?,
        id,
        af_project::Wait::Until(std::time::Instant::now() + std::time::Duration::from_secs(1)),
    )
    .map_err(af_project::Unopened::message)
}

pub(super) fn command(command: Command, inspect: &Inspect) -> Result<i32> {
    match command {
        Command::Validate { receiver } => {
            let project = af_project::require_checkout(&receiver.repo)?;
            print(&review(receiver, inspect, &project.project_id)?, inspect)
        }
        Command::Enroll {
            receiver,
            confirm_pin,
        } => {
            let project = af_project::require_checkout(&receiver.repo)?;
            let enrollment = review(receiver, inspect, &project.project_id)?;
            require(
                confirm_pin == enrollment.revision,
                "Backlog enrollment pin confirmation differs",
            )?;
            let store = Store::open(
                Arc::new(AfHost),
                Some(&enrollment.scope.scenario_state),
                true,
            )?;
            let mut project = write_project(&project.project_id)?;
            let current = project.backlog_schedule_scope()?;
            enrollment.scope.validate_project(&current)?;
            enrollment.scope.validate_receiver()?;
            if let Some(old) = load(&project.directory)? {
                if old.revision == enrollment.revision {
                    return print(&old, inspect);
                }
                require(
                    old.paused && old.scope.scenario_state == store.path(),
                    "Pause the enrollment in its original Scenario Store before replacing its scope",
                )?;
                require(
                    !store.read()?.runs.values().any(|run| {
                        run.status.active()
                            && recurring_template(run)
                                .is_some_and(|template| template.repo == old.scope.repo)
                    }),
                    "Outstanding recurring backlog Run must settle before replacing enrollment",
                )?;
            }
            require(
                !current.active,
                "Settle the active Project batch before enrolling new authority",
            )?;
            let scope = &enrollment.scope;
            project.tick_backlog_schedule(
                at(inspect)?,
                &scope.batch_policy,
                &scope.intake_policy,
                &scope.destination,
                true,
            )?;
            save(&project, &enrollment)?;
            print(&enrollment, inspect)
        }
        Command::Show { repo } => {
            let project = read_project(&af_project::require_checkout(&repo)?.project_id)?;
            let enrollment = load(&project.directory)?.ok_or("Backlog schedule is not enrolled")?;
            enrollment.scope.validate_store(inspect.state.as_deref())?;
            print(&enrollment, inspect)
        }
        Command::Pause { ref repo } | Command::Resume { ref repo } => {
            let paused = matches!(command, Command::Pause { .. });
            let mut project = write_project(&af_project::require_checkout(repo)?.project_id)?;
            let mut enrollment =
                load(&project.directory)?.ok_or("Backlog schedule is not enrolled")?;
            enrollment.scope.validate_store(inspect.state.as_deref())?;
            if enrollment.paused != paused {
                if !paused {
                    let scope = &enrollment.scope;
                    scope.validate_receiver()?;
                    // Consume any due paused slot under the Project lock before enabling.
                    project.tick_backlog_schedule(
                        at(inspect)?,
                        &scope.batch_policy,
                        &scope.intake_policy,
                        &scope.destination,
                        true,
                    )?;
                }
                enrollment.paused = paused;
                save(&project, &enrollment)?;
            }
            print(&enrollment, inspect)
        }
    }
}

fn run_id(reference: &BatchReference) -> String {
    format!(
        "{RUN_PREFIX}{}",
        reference.batch_id.trim_start_matches("sha256:")
    )
}

pub(super) fn reserved(id: &str) -> bool {
    id.starts_with(RUN_PREFIX)
}

fn recurring_template(run: &Run) -> Option<&Template> {
    if !reserved(&run.id) {
        return None;
    }
    run.pin.bundle.definitions[&run.pin.bundle.root]
        .steps
        .iter()
        .find_map(|step| match &step.operation {
            Kind::FreshTask { template, .. } => Some(template.as_ref()),
            _ => None,
        })
}

fn batch_policy(bundle: &Bundle) -> Policy {
    Policy {
        schema: "af/scenario-policy@8".into(),
        task_templates: bundle.definitions[&bundle.root]
            .authority
            .task_templates
            .clone(),
        adapters: BTreeMap::new(),
        task_plans: BTreeSet::new(),
        decision_keys: BTreeMap::new(),
        nightly_releases: BTreeMap::new(),
        performance_publications: BTreeMap::new(),
        file_roots: BTreeSet::new(),
        implementation_startup: None,
    }
}

pub(super) fn check_dispatch(state: &Path, run: &Run) -> Result<()> {
    if !reserved(&run.id) {
        return Ok(());
    }
    let template = recurring_template(run).ok_or("Missing recurring backlog Task template")?;
    let reference = portable::backlog_template_reference(template)?;
    reference.validate()?;
    let project = read_project(&reference.project_id)?;
    let enrollment = load(&project.directory)?.ok_or("Backlog schedule is not enrolled")?;
    let scope = &enrollment.scope;
    require(
        state == scope.scenario_state
            && run.id == run_id(&reference)
            && portable::backlog_template_enrollment(template) == Some(&enrollment.revision)
            && run.pin.policy == batch_policy(&run.pin.bundle),
        "Recurring backlog Run is outside its enrolled Store, batch or Scenario policy",
    )?;
    scope.validate_project(&project.backlog_schedule_scope()?)?;
    scope.validate_receiver()?;
    scope.validate_bundle(&run.pin.bundle)?;
    project.check_backlog_action_destination(&reference, &scope.destination)
}

fn admit(store: &Store, enrollment: Enrollment, now: u64) -> Result<Option<String>> {
    let scope = &enrollment.scope;
    let mut project = write_project(&scope.project_id)?;
    let current = load(&project.directory)?.ok_or("Backlog schedule is not enrolled")?;
    require(
        current.revision == enrollment.revision,
        "Backlog enrollment changed during tick",
    )?;
    scope.validate_receiver()?;
    let reference = project.tick_backlog_schedule(
        now,
        &scope.batch_policy,
        &scope.intake_policy,
        &scope.destination,
        current.paused,
    )?;
    drop(project);
    let Some(reference) = reference else {
        return Ok(None);
    };
    let id = run_id(&reference);
    let world = store.read()?;
    if let Some(run) = world.runs.get(&id) {
        require(
            run.status.active(),
            "Frozen backlog batch has a terminal Run; inspect the batch and Run for recovery",
        )?;
        return Ok(None);
    }
    let bundle = portable::prepare_backlog(
        scope.repo.clone(),
        scope.task_state.clone(),
        &scope.file,
        reference.clone(),
        scope.bindings_path(),
        &enrollment.revision,
    )?;
    require(
        scope.validate_bundle(&bundle)? == reference,
        "Prepared backlog batch reference changed",
    )?;
    let policy = batch_policy(&bundle);
    let pin = Pin::new(&AfHost, bundle, policy)?;
    let run = Run::new(
        &AfHost,
        id.clone(),
        pin,
        BTreeMap::new(),
        scope.limits.clone(),
        now,
        None,
    )?;
    check_dispatch(store.path(), &run)?;
    store.update(|world| {
        if world.runs.contains_key(&id) {
            return Ok(None);
        }
        world.admit_run(run)?;
        Ok(Some(id))
    })
}

pub(super) fn tick(store: &Store, now: u64, clock: Option<u64>) -> Result<runtime::Tick> {
    let mut tick = runtime::tick(store, now, clock)?;
    let root = af_project::projects_root(&crate::xdg_state_root()?);
    let directories = match fs::read_dir(root) {
        Ok(directories) => Some(directories),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let mut directories = directories
        .into_iter()
        .flatten()
        .map(|entry| {
            let entry = entry.map_err(|e| e.to_string())?;
            Ok((
                entry.path(),
                entry.file_type().map_err(|e| e.to_string())?.is_dir(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    directories.sort();
    for (path, is_dir) in directories {
        if !is_dir {
            continue;
        }
        let result = load(&path).and_then(|enrollment| match enrollment {
            Some(enrollment) if enrollment.scope.scenario_state == store.path() => {
                admit(store, enrollment, now)
            }
            _ => Ok(None),
        });
        match result {
            Ok(Some(id)) => tick.started.push(id),
            Ok(None) => {}
            Err(error) => {
                if let Some(id) = path.file_name().and_then(|id| id.to_str()) {
                    tick.refused.insert(format!("project-backlog-{id}"), error);
                }
            }
        }
    }
    for (id, run) in store.read()?.runs {
        if !reserved(&id) || !run.status.active() || run.updated_unix_ms > now {
            continue;
        }
        match runtime::resume(store, &id, clock) {
            Ok(run) => {
                tick.runs.insert(id, run);
            }
            Err(error) if error.starts_with("Scenario Run is already executing:") => {
                tick.busy.push(id)
            }
            Err(error) => {
                tick.refused.insert(id, error);
            }
        }
    }
    Ok(tick)
}
