//! Fresh token-free account identity proof for Task bindings. Capability probes are dispatched
//! later as paid nodes by the common Task runtime. Credentials and raw account data stay local.
//!
//! Admission first checks, token-free, that the installed CLI accepts af's invocation
//! ([`check_invocation`], ADR-0146), so an authenticated but incompatible CLI is refused before
//! any Worker or admission Attempt starts and before its account is asked about. The Codex
//! identity probe then proves the app-server protocol through its own `initialize` round trip; an
//! app-server that exits before answering it is refused as the missing protocol capability.
use super::compatibility::protocol_incompatibility;
use super::*;
use review_core::task::plan::WorkerExecutionV1;
use review_core::task::provider_event::{ProbeSilenceV1, ProviderEventV1};
use review_runner::native_failure::NativeFailureKind;
use review_runner::task::{ModelWorkerReturn, Unstarted, WorkerModelAdapter};
use review_runner_claude::models as rules;
use std::sync::Arc;

#[derive(Clone)]
pub struct TaskProviderIdentity {
    spec: ProviderSpec,
    program: PathBuf,
    principal_id: String,
    auth_method: String,
    /// A digest of the organization the account answered for, when the CLI names one.
    org: Option<String>,
    /// The CLI's own `--version` line, once a recheck read it, and the executable file the
    /// capture follows: an update that repoints the launcher is read then, and never mistaken
    /// for an account change.
    cli_version: Option<String>,
    binary: Option<BinaryStamp>,
    /// The most one identity probe of a recheck may take; None gives each probe its own
    /// timeout. Every probe is bounded by the Attempt's wall either way.
    probe_wall: Option<Duration>,
    probe_path: std::ffi::OsString,
    home: Option<std::ffi::OsString>,
    user: Option<std::ffi::OsString>,
}

/// Which file an executable path resolves to, and when it last changed.
#[derive(Clone, PartialEq, Eq)]
struct BinaryStamp {
    target: PathBuf,
    inode: u64,
    modified: i64,
}

impl BinaryStamp {
    fn of(program: &Path) -> Option<Self> {
        let target = std::fs::canonicalize(program).ok()?;
        let metadata = std::fs::metadata(&target).ok()?;
        Some(Self {
            target,
            inode: metadata.ino(),
            modified: metadata.mtime(),
        })
    }
}

/// The CLI's `--version` line, when it answers one before `deadline`.
fn cli_version(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    deadline: Option<Instant>,
) -> Option<String> {
    match installation::version_check(program, spec, probe_path, cancelled, deadline) {
        installation::VersionCheck::Reported(line) => line
            .map(|line| {
                line.chars()
                    .filter(|c| c.is_ascii_graphic() || *c == ' ')
                    .take(128)
                    .collect::<String>()
            })
            .filter(|line| !line.trim().is_empty()),
        _ => None,
    }
}

/// The authentication method class a Claude or Codex login answers with. Claude Code spells one
/// subscription login `claude.ai` or `oauth` across releases; both are the same method.
fn method_class(method: &str) -> &str {
    match method {
        "claude.ai" | "oauth" => "oauth",
        other => other,
    }
}
impl TaskProviderIdentity {
    /// The token-free static rules of a Model binding on Provider `provider` of kind `kind`, from
    /// the one validation authority, `review_runner_claude::models` (ADR-0148): grammar G on the
    /// model and the effort level first (ADR-0147), so a value outside G is refused with G's words
    /// whatever else it fails; a documented alias, refused with its exact ID; a case or separator
    /// variant of a known exact ID; the versioned-ID rule; the kind's effort vocabulary.
    /// [`Self::execution`] applies them, and the binders apply them to every Model Worker in a
    /// first pass before any [`Self::probe`]. A refusal is the hint naming the Provider, the
    /// rejected spelling, the rule, the nearest accepted spellings and `af provider models <id>`.
    pub fn check_static_rules(
        provider: &str,
        kind: &str,
        model: &str,
        effort: &str,
    ) -> Result<(), String> {
        let kind = rules::Kind::parse(kind)
            .ok_or("Task Model binding names an unsupported Provider kind")?;
        rules::check_static(kind, model, Some(effort)).map_err(|refusal| {
            refusal.hint(
                rules::ProviderRef {
                    id: Some(provider),
                    kind,
                },
                model,
                Some(effort),
            )
        })
    }
    pub fn probe(provider: &str, expected_kind: &str) -> Result<Self, String> {
        let spec = configured_spec(provider)?;
        if spec.kind.name() != expected_kind || spec.auth_dir.is_none() {
            return Err("Task Provider binding differs from its configured implementation".into());
        }
        // A CLI that cannot start, or does not accept af's invocation, is refused here, before
        // any Worker or admission Attempt runs and before its account is asked about.
        let program = locate_cli(&spec)
            .map_err(|failure| admission_refusal(&CliRefusal::CannotStart(failure)))?;
        let probe_path = sanitized_path();
        let cancelled = AtomicBool::new(false);
        check_invocation(&program, &spec, &probe_path, &cancelled, None)
            .map_err(|refusal| admission_refusal(&refusal))?;
        let observed = observe_identity(&program, &spec, &probe_path, &cancelled, None).map_err(
            |failure| match cli_cannot_serve(
                &program,
                &spec,
                &probe_path,
                &cancelled,
                None,
                &failure,
            ) {
                Some(refusal) => admission_refusal(&refusal),
                None => failure.message,
            },
        )?;
        // Which file the executable is, without starting it again: its version is read only
        // once that file changes, so capture spends no process on a CLI that never updates.
        let binary = BinaryStamp::of(&program);
        Ok(Self {
            spec,
            program,
            principal_id: observed.principal,
            auth_method: observed.auth_method,
            org: observed.org,
            cli_version: None,
            binary,
            probe_wall: None,
            probe_path,
            home: std::env::var_os("HOME"),
            user: std::env::var_os("USER"),
        })
    }
    pub fn execution(&self, model: &str, effort: &str) -> Result<WorkerExecutionV1, String> {
        Self::check_static_rules(&self.spec.id, self.spec.kind.name(), model, effort)?;
        let execution = WorkerExecutionV1::Model {
            provider: self.spec.id.clone(),
            provider_kind: self.spec.kind.name().into(),
            principal_id: self.principal_id.clone(),
            model: model.into(),
            effort: effort.into(),
        };
        execution.validate()?;
        Ok(execution)
    }
    pub fn adapter(
        &self,
        execution: &WorkerExecutionV1,
    ) -> Result<Box<dyn WorkerModelAdapter>, String> {
        let WorkerExecutionV1::Model { model, effort, .. } = execution else {
            return Err("A Command binding has no Model adapter".into());
        };
        if &self.execution(model, effort)? != execution {
            return Err("Current Provider account differs from the captured Task binding".into());
        }
        Ok(Box::new(CurrentTaskProviderAdapter {
            model: model.clone(),
            effort: effort.clone(),
            resolve: resolve_program,
            current: Mutex::new((self.clone(), self.native_adapter(model, effort)?.into())),
        }))
    }

    /// The native adapter that runs this identity's executable under its authentication grants.
    fn native_adapter(
        &self,
        model: &str,
        effort: &str,
    ) -> Result<Box<dyn WorkerModelAdapter>, String> {
        let program = self
            .program
            .to_str()
            .ok_or("Provider executable path is not UTF-8")?;
        let auth = self
            .spec
            .auth_dir
            .as_ref()
            .and_then(|p| p.to_str())
            .ok_or("Provider auth directory is not absolute UTF-8")?;
        let flags = match self.spec.kind {
            ProviderKind::Claude => vec![
                "--model".into(),
                model.into(),
                "--effort".into(),
                effort.into(),
            ],
            ProviderKind::Codex => vec![
                "--model".into(),
                model.into(),
                "-c".into(),
                format!(
                    "model_reasoning_effort={}",
                    serde_json::to_string(effort).map_err(|e| e.to_string())?
                ),
            ],
        };
        let command = review_core::Command::new(
            program,
            flags.into_iter().map(review_core::Arg::literal).collect(),
        );
        Ok(match self.spec.kind {
            ProviderKind::Claude => Box::new(
                review_runner_claude::task::ClaudeTaskAdapter::new(&command)?
                    .for_provider(self.spec.id.clone())
                    .with_auth(
                        Some(auth.into()),
                        self.user
                            .as_ref()
                            .and_then(|s| s.to_str())
                            .ok_or("Claude Provider requires USER")?
                            .into(),
                        self.home
                            .as_ref()
                            .and_then(|s| s.to_str())
                            .ok_or("Claude Provider requires HOME")?
                            .into(),
                    )
                    // Each Attempt's `projects/<slug>` history goes when its process exits,
                    // unless the machine keeps it (ADR-0144).
                    .keeping_transcripts(
                        crate::storage::policy().is_ok_and(|policy| policy.keep_worker_transcripts),
                    ),
            ),
            ProviderKind::Codex => Box::new(
                review_runner_codex::task::CodexTaskAdapter::new(&command)?
                    .with_codex_home(auth.into()),
            ),
        })
    }

    /// The executable is the one this identity names: every invocation runs that absolute path,
    /// so it must still be there. What `PATH` resolves to now is not compared. A native client
    /// that updates itself repoints its launcher at a new version file (ADR-0126): the version it
    /// reports then is read again, and an update alone is recorded, never refused (ADR-0202).
    /// What the recheck did about the Provider without refusing is returned as Task events, with
    /// the executable and CLI version the capture follows from then on.
    fn check_current(
        &self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Rechecked, Recheck> {
        check_task_probe_control(Some(deadline), cancelled).map_err(|_| Recheck::NotCurrent)?;
        let current = configured_spec(&self.spec.id).map_err(|_| Recheck::NotCurrent)?;
        if current.kind != self.spec.kind
            || current.auth_dir != self.spec.auth_dir
            || current.explicit_selector != self.spec.explicit_selector
            || !is_executable(&self.program)
            || sanitized_path() != self.probe_path
            || std::env::var_os("HOME") != self.home
            || std::env::var_os("USER") != self.user
        {
            return Err(Recheck::NotCurrent);
        }
        self.recheck_account(deadline, cancelled)
    }

    /// The account recheck of [`Self::check_current`]: up to [`IDENTITY_PROBES`] probes within
    /// the Attempt's wall, then the CLI version the capture follows.
    fn recheck_account(
        &self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Rechecked, Recheck> {
        let mut rechecked = Rechecked::default();
        // A probe that proves nothing either way is sent again within the Attempt's wall:
        // silence on a busy host is never an account change (ADR-0202).
        let mut silences = Vec::new();
        loop {
            let probe_deadline = self
                .probe_wall
                .map_or(deadline, |wall| deadline.min(Instant::now() + wall));
            match self.recheck_identity(probe_deadline, cancelled) {
                Ok(()) => break,
                Err(Recheck::Silent(cause)) => {
                    silences.push(cause);
                    let probe = silences.len() as u32 + 1;
                    if probe > IDENTITY_PROBES
                        || check_task_probe_control(Some(deadline), cancelled).is_err()
                    {
                        return Err(Recheck::Unverified {
                            silences,
                            events: rechecked.events,
                        });
                    }
                    rechecked
                        .events
                        .push(ProviderEventV1::IdentityReprobed { cause, probe });
                }
                Err(other) => return Err(other),
            }
        }
        let stamp = BinaryStamp::of(&self.program);
        if stamp != self.binary {
            let version = cli_version(
                &self.program,
                &self.spec,
                &self.probe_path,
                cancelled,
                Some(deadline),
            );
            if let Some(version) = version.filter(|v| self.cli_version.as_ref() != Some(v)) {
                rechecked.events.push(ProviderEventV1::IdentityRefreshed {
                    provider: self.spec.id.clone(),
                    previous_version: self.cli_version.clone().unwrap_or_else(|| "unknown".into()),
                    current_version: version.clone(),
                });
                rechecked.cli_version = Some(version);
            }
            rechecked.binary = Some(stamp);
        }
        Ok(rechecked)
    }

    /// The token-free account recheck, telling a CLI that cannot start, an account that changed
    /// and a probe that proved nothing apart.
    fn recheck_identity(&self, deadline: Instant, cancelled: &AtomicBool) -> Result<(), Recheck> {
        let observed = observe_identity(
            &self.program,
            &self.spec,
            &self.probe_path,
            cancelled,
            Some(deadline),
        )
        .map_err(|failure| {
            match cli_cannot_serve(
                &self.program,
                &self.spec,
                &self.probe_path,
                cancelled,
                Some(deadline),
                &failure,
            ) {
                Some(refusal) => Recheck::CliCannotServe(refusal),
                None => match failure.silence {
                    Some(cause) => Recheck::Silent(cause),
                    None => Recheck::NotCurrent,
                },
            }
        })?;
        check_task_probe_control(Some(deadline), cancelled).map_err(|_| Recheck::NotCurrent)?;
        if observed.principal != self.principal_id {
            return Err(Recheck::Changed("account"));
        }
        if method_class(&observed.auth_method) != method_class(&self.auth_method) {
            return Err(Recheck::Changed("authentication method"));
        }
        if observed.org.is_some() && self.org.is_some() && observed.org != self.org {
            return Err(Recheck::Changed("organization"));
        }
        Ok(())
    }
}

/// How many identity probes one recheck sends before it gives up proving the account.
const IDENTITY_PROBES: u32 = 3;

/// What a passed recheck observed beyond the account: the events it did about the Provider, and
/// the executable and CLI version the capture follows from now on.
#[derive(Default)]
struct Rechecked {
    events: Vec<ProviderEventV1>,
    binary: Option<Option<BinaryStamp>>,
    cli_version: Option<String>,
}

/// Why the recheck before a private send refused it.
enum Recheck {
    /// The local authority the identity was captured under changed: the registry entry, the
    /// executable, `PATH`, `HOME` or `USER`.
    NotCurrent,
    /// The account answered, and it is another account, method or organization.
    Changed(&'static str),
    /// One probe proved nothing either way.
    Silent(ProbeSilenceV1),
    /// Every probe proved nothing: an environment failure, not an account change. `events` are
    /// the re-probes sent before giving up.
    Unverified {
        silences: Vec<ProbeSilenceV1>,
        events: Vec<ProviderEventV1>,
    },
    /// The Provider's CLI cannot start, or its app-server no longer answers `initialize`: an
    /// environment failure, not the model's or the login's.
    CliCannotServe(CliRefusal),
}

/// The 1-minute load average, where the platform reports it.
fn host_load() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

impl Recheck {
    /// The Attempt diagnostic of a refused recheck, naming its cause and what to do. `probe_wall`
    /// is the identity's own bound on one probe, when it has one.
    fn diagnostic(&self, kind: ProviderKind, probe_wall: Option<Duration>) -> String {
        let client = match kind {
            ProviderKind::Claude => "Claude",
            ProviderKind::Codex => "Codex",
        };
        match self {
            Recheck::NotCurrent => concat!(
                "Captured Task Provider identity is no longer current: its registry entry, ",
                "executable, PATH, HOME or USER changed since the plan; run `af task plan` again"
            )
            .into(),
            Recheck::Changed(what) => format!(
                "identity recheck: the {client} {what} differs from the one captured at plan: run \
                 `af task plan` again"
            ),
            Recheck::Silent(cause) => format!(
                "identity recheck: the probe {}; the account was not verified as changed",
                cause.as_str()
            ),
            Recheck::Unverified { silences, .. } => {
                let causes: BTreeSet<&str> = silences.iter().map(|cause| cause.as_str()).collect();
                let cause = causes.into_iter().collect::<Vec<_>>().join(" or ");
                let each = probe_wall.unwrap_or_else(|| {
                    probe_timeout(match kind {
                        ProviderKind::Claude => CLAUDE_STRUCTURAL_TIMEOUT,
                        ProviderKind::Codex => PROBE_TIMEOUT,
                    })
                });
                let each = if silences.contains(&ProbeSilenceV1::TimedOut) {
                    format!(" (up to {} s each)", each.as_secs().max(1))
                } else {
                    String::new()
                };
                let load = host_load()
                    .map(|load| format!(" on a host with load {load:.0}"))
                    .unwrap_or_default();
                format!(
                    "identity recheck: probe {cause} {}×{each}{load}; the account was not \
                     verified as changed: {}; run the Task again with `af task run`",
                    silences.len(),
                    NativeFailureKind::Environment.diagnostic()
                )
            }
            Recheck::CliCannotServe(refusal) => environment_failure(refusal),
        }
    }
}

/// Why an account identity probe proved nothing.
struct IdentityFailure {
    message: String,
    /// The probe proved nothing either way: it timed out, or answered a line af cannot read.
    silence: Option<ProbeSilenceV1>,
    /// The CLI gave no answer af recognizes, and af did not give up waiting for one: it may not
    /// start at all. A CLI that answered — logged out, another account — never is.
    cli_silent: bool,
    /// The Codex app-server gave no `initialize` result object — it exited first, rejected it or
    /// answered it malformed — or wrote a malformed line before the identity answer: unless the
    /// CLI cannot start at all, it lacks the protocol capability the probe needs.
    protocol_unanswered: bool,
}

impl IdentityFailure {
    fn answered(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            silence: None,
            cli_silent: false,
            protocol_unanswered: false,
        }
    }

    /// An answer af cannot read: it proves nothing about the account.
    fn malformed(message: impl Into<String>) -> Self {
        Self {
            silence: Some(ProbeSilenceV1::Malformed),
            ..Self::answered(message)
        }
    }
}

/// What one identity probe observed.
struct Observation {
    principal: String,
    auth_method: String,
    org: Option<String>,
}

/// A digest of an organization identity, so neither it nor its name is kept.
fn org_digest(kind: &str, org: Option<&str>) -> Option<String> {
    let org = org?.trim();
    if org.is_empty() {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"af/provider-org/v1\0");
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(org.as_bytes());
    Some(format!(
        "sha256:{}",
        review_core::hex::encode(&digest.finalize())
    ))
}

/// Why a silent identity probe's CLI cannot serve af: an installation failure when its own
/// version check confirms one, else, for an app-server that gave no `initialize` result object,
/// the missing protocol capability.
fn cli_cannot_serve(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    deadline: Option<Instant>,
    failure: &IdentityFailure,
) -> Option<CliRefusal> {
    if !failure.cli_silent {
        return None;
    }
    if failure.protocol_unanswered {
        return Some(protocol_incompatibility(
            program,
            spec,
            probe_path,
            cancelled,
            deadline,
            &failure.message,
        ));
    }
    diagnose_cli(program, spec, probe_path, cancelled, deadline).map(CliRefusal::CannotStart)
}

fn admission_refusal(refusal: &CliRefusal) -> String {
    format!(
        "Task Provider admission refused before any Worker was dispatched: {}",
        refusal.message()
    )
}

/// The Attempt diagnostic for a CLI that stopped starting, or serving its protocol, after
/// admission.
fn environment_failure(refusal: &CliRefusal) -> String {
    format!(
        "Provider environment failure, not a model or credential failure: {}",
        refusal.message()
    )
}

#[cfg(test)]
fn probe_identity(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<(String, String), String> {
    observe_identity(program, spec, probe_path, cancelled, deadline)
        .map(|observed| (observed.principal, observed.auth_method))
        .map_err(|f| f.message)
}

/// Whether a probe error says it ran out of its own time, not that it was cancelled.
fn probe_timed_out(message: &str) -> bool {
    message.contains("timed out")
}

fn observe_identity(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<Observation, IdentityFailure> {
    match spec.kind {
        ProviderKind::Claude => {
            let output = run_probe_before(program, spec, probe_path, cancelled, deadline).map_err(
                |message| IdentityFailure {
                    cli_silent: !probe_gave_up(&message),
                    silence: probe_timed_out(&message).then_some(ProbeSilenceV1::TimedOut),
                    message,
                    protocol_unanswered: false,
                },
            )?;
            if !output.status.success() {
                let answered = status_answered(spec.kind, &output.stdout);
                return Err(IdentityFailure {
                    message: "Claude account identity probe failed".into(),
                    cli_silent: !answered,
                    silence: (!answered).then_some(ProbeSilenceV1::Malformed),
                    protocol_unanswered: false,
                });
            }
            let status: serde_json::Value = serde_json::from_str(&output.stdout).map_err(|_| {
                IdentityFailure::malformed("Claude account identity response is invalid")
            })?;
            let principal = claude_principal(&status).map_err(IdentityFailure::answered)?;
            Ok(Observation {
                principal,
                auth_method: status["authMethod"].as_str().unwrap().into(),
                org: org_digest("claude", status["orgId"].as_str()),
            })
        }
        ProviderKind::Codex => {
            let mut observed = AppServerProbe::default();
            let response = probe_codex_request_observed(
                program,
                spec,
                probe_path,
                cancelled,
                Some(&serde_json::json!({"method":"account/read","id":2,"params":{"refreshToken":false}})),
                deadline,
                &mut observed,
            )
            .map_err(|message| IdentityFailure {
                // A malformed line, before or after the `initialize` answer, is no answer af
                // recognizes either.
                cli_silent: observed.lacks_protocol
                    || !observed.answered && !probe_gave_up(&message),
                silence: if probe_timed_out(&message) {
                    Some(ProbeSilenceV1::TimedOut)
                } else {
                    observed.lacks_protocol.then_some(ProbeSilenceV1::Malformed)
                },
                protocol_unanswered: observed.lacks_protocol,
                message,
            })?;
            let principal = codex_principal(&response).map_err(IdentityFailure::answered)?;
            Ok(Observation {
                principal,
                auth_method: "chatgpt".into(),
                org: None,
            })
        }
    }
}

/// A token-free recheck before each private send. The status process and native invocation share
/// the original remaining Attempt wall limit and cancellation. This detects between-node drift;
/// credentials may still change between the check and the native client's auth consumption.
struct CurrentTaskProviderAdapter {
    model: String,
    effort: String,
    /// How a Provider command is found on `PATH`; a test supplies its own.
    resolve: fn(&str) -> Option<PathBuf>,
    /// The identity in use and the native adapter that runs its executable.
    current: Mutex<(TaskProviderIdentity, Arc<dyn WorkerModelAdapter>)>,
}
/// An update removes at most a few versions while one Worker waits to start.
const MAX_EXECUTABLE_REPLACEMENTS: usize = 3;

impl CurrentTaskProviderAdapter {
    /// The captured executable while it is there. A native client update can remove it; the
    /// client `PATH` resolves now then takes its place for the rest of the process, and must
    /// pass the same identity recheck before it receives anything (ADR-0126). The error is the
    /// refusal to report.
    fn current_or_installed(
        &self,
    ) -> Result<(TaskProviderIdentity, Arc<dyn WorkerModelAdapter>), &'static str> {
        let mut current = self.current.lock().expect("Task Provider adapter");
        if !is_executable(&current.0.program) {
            let mut identity = current.0.clone();
            identity.program = (self.resolve)(identity.spec.kind.command()).ok_or(concat!(
                "Captured Task Provider executable was removed and no installed client ",
                "replaces it"
            ))?;
            let native = identity.native_adapter(&self.model, &self.effort).map_err(
                |_| "Installed Task Provider client cannot replace the removed executable",
            )?;
            *current = (identity, native.into());
        }
        Ok(current.clone())
    }

    /// The kind of the Provider this adapter runs for.
    fn native_kind(&self) -> ProviderKind {
        self.current
            .lock()
            .expect("Task Provider adapter")
            .0
            .spec
            .kind
    }

    /// After a passed recheck the capture follows the executable and CLI version it observed:
    /// an update is recorded once, not again before every later send.
    fn follow(&self, identity: &TaskProviderIdentity, rechecked: &Rechecked) {
        let mut current = self.current.lock().expect("Task Provider adapter");
        if current.0.program != identity.program {
            return;
        }
        if let Some(binary) = &rechecked.binary {
            current.0.binary = binary.clone();
        }
        if let Some(version) = &rechecked.cli_version {
            current.0.cli_version = Some(version.clone());
        }
    }

    fn native(&self) -> Arc<dyn WorkerModelAdapter> {
        self.current
            .lock()
            .expect("Task Provider adapter")
            .1
            .clone()
    }
}
impl WorkerModelAdapter for CurrentTaskProviderAdapter {
    fn credential_mode(&self) -> review_core::CredentialModeV1 {
        self.native().credential_mode()
    }
    fn provider_kind(&self) -> &'static str {
        self.native().provider_kind()
    }
    fn model_settings(&self) -> Option<(String, String)> {
        self.native().model_settings()
    }
    /// Sandbox-local environment (a carried Build Cache location) and the kernel-derived access
    /// are forwarded to the native client exactly as they arrived, and only after the identity
    /// recheck passes.
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: review_runner::task::WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        let refuse = |reason: &str| ModelWorkerReturn {
            notes: Default::default(),
            message: Err(reason.into()),
            usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
            usage_observation: None,
            raw_artifact_ids: vec![],
            native_failure: None,
        };
        let refused = || refuse(&Recheck::NotCurrent.diagnostic(self.native_kind(), None));
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return refused();
        };
        let local = AtomicBool::new(false);
        let cancelled = cancellation.unwrap_or(&local);
        // An update can remove the executable in use while it is rechecked, or between the
        // recheck and its start. Each time the installed client takes its place and is
        // rechecked; an executable that could not be started received no input.
        for _ in 0..=MAX_EXECUTABLE_REPLACEMENTS {
            // Named apart from an identity change: the Provider's client cannot be run at all.
            let (identity, native) = match self.current_or_installed() {
                Ok(current) => current,
                Err(reason) => return refuse(reason),
            };
            // Raw status/account output and filesystem diagnostics remain local, never Worker
            // evidence. A CLI that cannot start is named as the environment failure it is, so
            // neither retries nor reports blame the model or the login; its diagnostic is never
            // retry feedback.
            let rechecked = match identity.check_current(deadline, cancelled) {
                Ok(rechecked) => rechecked,
                Err(_) if !is_executable(&identity.program) => continue,
                Err(recheck) => {
                    let mut returned =
                        refuse(&recheck.diagnostic(identity.spec.kind, identity.probe_wall));
                    // A CLI that cannot serve and a probe that proved nothing are the host's
                    // failure, never an account change (ADR-0202).
                    if matches!(
                        recheck,
                        Recheck::CliCannotServe(_) | Recheck::Unverified { .. }
                    ) {
                        returned.native_failure = Some(NativeFailureKind::Environment);
                    }
                    if let Recheck::Unverified { events, .. } = recheck {
                        returned.notes.events = events;
                    }
                    return returned;
                }
            };
            self.follow(&identity, &rechecked);
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return refused();
            };
            let mut returned = match native.invoke_started(
                cas,
                workdir,
                input.clone(),
                remaining,
                access,
                cancellation,
                environment,
            ) {
                Ok(returned) => returned,
                Err(Unstarted(_)) if !is_executable(&identity.program) => continue,
                Err(Unstarted(returned)) => *returned,
            };
            let mut events = rechecked.events;
            events.append(&mut returned.notes.events);
            returned.notes.events = events;
            return returned;
        }
        refused()
    }
}

fn principal(kind: &str, email: Option<&str>) -> Result<String, String> {
    let email = email.ok_or(
        "Provider did not expose an account identity; aliases cannot establish independence",
    )?;
    let normalized = email.trim().to_ascii_lowercase();
    let (user, domain) = normalized
        .rsplit_once('@')
        .ok_or("Provider account identity is malformed")?;
    if user.is_empty()
        || domain.is_empty()
        || normalized.len() > 320
        || normalized.chars().any(char::is_whitespace)
        || normalized.chars().any(char::is_control)
    {
        return Err("Provider account identity is malformed".into());
    }
    let mut digest = Sha256::new();
    digest.update(b"af/provider-principal/email/v1\0");
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(normalized.as_bytes());
    Ok(format!(
        "sha256:{}",
        review_core::hex::encode(&digest.finalize())
    ))
}
fn claude_principal(status: &serde_json::Value) -> Result<String, String> {
    if status["loggedIn"] != true
        || status["apiProvider"] != "firstParty"
        || !matches!(status["authMethod"].as_str(), Some("claude.ai" | "oauth"))
    {
        return Err(
            "Claude Task binding requires a first-party authenticated account identity".into(),
        );
    }
    principal("claude", status["email"].as_str())
}
fn codex_principal(response: &serde_json::Value) -> Result<String, String> {
    if response.get("error").is_some() || response["result"]["account"]["type"] != "chatgpt" {
        return Err(
            "Codex Task binding requires an authenticated account with identity proof".into(),
        );
    }
    principal("codex", response["result"]["account"]["email"].as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn aliases_and_auth_directories_do_not_create_another_principal() {
        let a = json!({"loggedIn":true,"apiProvider":"firstParty","authMethod":"claude.ai","email":"Developer@Example.test"});
        let mut b = a.clone();
        b["email"] = json!("developer@example.test");
        b["alias"] = json!("another-local-label");
        b["orgId"] = json!("another-org");
        assert_eq!(claude_principal(&a).unwrap(), claude_principal(&b).unwrap());
        assert!(!claude_principal(&a).unwrap().contains("example"));
        b["loggedIn"] = json!(false);
        assert!(claude_principal(&b).is_err());
        let codex = json!({"result":{"account":{"type":"chatgpt","email":"developer@example.test","planType":"pro"}}});
        assert_ne!(
            claude_principal(&a).unwrap(),
            codex_principal(&codex).unwrap()
        );
        for account in [
            json!(null),
            json!({"type":"apiKey"}),
            json!({"type":"chatgpt","email":null}),
            json!({"type":"chatgpt","email":""}),
        ] {
            assert!(codex_principal(&json!({"result":{"account":account}})).is_err());
        }
    }

    /// Grammar G is checked before every other rule: a value outside G is refused with G's words
    /// and its byte length whatever else it fails. The other static rules follow, each refusal
    /// naming the Provider, the rule, the nearest accepted spellings and `af provider models`.
    #[test]
    fn execution_applies_grammar_g_before_the_other_static_rules() {
        let identity = TaskProviderIdentity {
            spec: ProviderSpec {
                id: "fixture".into(),
                kind: ProviderKind::Claude,
                auth_dir: Some(PathBuf::from("/nonexistent")),
                explicit_selector: true,
                registry_declared: true,
                source: "fixture".into(),
            },
            program: PathBuf::from("/nonexistent/claude"),
            principal_id: "sha256:".to_string() + &"0".repeat(64),
            auth_method: "claude.ai".into(),
            org: None,
            cli_version: None,
            binary: None,
            probe_wall: None,
            probe_path: std::ffi::OsString::from("/usr/bin:/bin"),
            home: None,
            user: None,
        };
        let long = "a".repeat(300);
        for (model, effort, option, bytes) in [
            ("bad@account", "high", "--model", 11),
            ("/Users/alice/keys", "high", "--model", 17),
            (long.as_str(), "high", "--model", 300),
            ("claude-fable-5-1", "x@y", "--effort", 3),
        ] {
            for refusal in [
                identity.execution(model, effort).unwrap_err(),
                TaskProviderIdentity::check_static_rules("fixture", "claude", model, effort)
                    .unwrap_err(),
            ] {
                assert!(
                    refusal.contains(&format!(
                        "(model_not_accepted): its {option} value ({bytes} bytes) is not a model \
                         spelling af accepts: {}",
                        review_runner_claude::hint::PRINTABLE_RULE
                    )),
                    "{refusal}"
                );
                assert!(
                    refusal.contains("for Provider fixture (claude)"),
                    "{refusal}"
                );
                for part in ["versioned", "bad", "account", "Users", "alice", "aaaa"] {
                    assert!(!refusal.contains(part), "{part}: {refusal}");
                }
            }
        }
        for model in ["claude-opus-latest", "claude-opus"] {
            let refusal = identity.execution(model, "high").unwrap_err();
            assert!(
                refusal.contains("Task Model binding requires an explicit versioned model ID"),
                "{refusal}"
            );
        }
        for (model, effort, named) in [
            ("fable", "xhigh", "the exact ID claude-fable-5-1"),
            (
                "claude-fable-5.1",
                "xhigh",
                "nearest accepted spellings: claude-fable-5-1",
            ),
            ("claude-fable-5-1", "ultra", "low, medium, high, xhigh, max"),
        ] {
            let refusal = identity.execution(model, effort).unwrap_err();
            assert!(refusal.contains(named), "{refusal}");
            assert!(
                refusal.contains("`af provider models fixture` (token-free)"),
                "{refusal}"
            );
        }
        assert!(identity.execution("claude-fable-5-1", "xhigh").is_ok());
        assert!(identity.execution("claude-opus-4-8", "max").is_ok());
    }
}

#[cfg(test)]
#[path = "task/currentness_tests.rs"]
mod currentness_tests;
