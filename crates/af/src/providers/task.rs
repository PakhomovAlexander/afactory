//! Fresh token-free account identity proof for Task bindings. Capability probes are dispatched
//! later as paid nodes by the common Task runtime. Credentials and raw account data stay local.
use super::*;
use review_core::task::plan::WorkerExecutionV1;
use review_runner::task::{ModelWorkerReturn, Unstarted, WorkerModelAdapter};
use std::sync::Arc;

#[derive(Clone)]
pub struct TaskProviderIdentity {
    spec: ProviderSpec,
    program: PathBuf,
    principal_id: String,
    auth_method: String,
    probe_path: std::ffi::OsString,
    home: Option<std::ffi::OsString>,
    user: Option<std::ffi::OsString>,
}
impl TaskProviderIdentity {
    pub fn probe(provider: &str, expected_kind: &str) -> Result<Self, String> {
        let spec = configured_spec(provider)?;
        if spec.kind.name() != expected_kind || spec.auth_dir.is_none() {
            return Err("Task Provider binding differs from its configured implementation".into());
        }
        // A CLI that cannot start is refused here, before any Worker or admission Attempt runs.
        let program = locate_cli(&spec).map_err(|failure| admission_refusal(&failure))?;
        let probe_path = sanitized_path();
        let cancelled = AtomicBool::new(false);
        let (principal_id, auth_method) = observe_identity(
            &program,
            &spec,
            &probe_path,
            &cancelled,
            None,
        )
        .map_err(|failure| {
            match cli_cannot_start(&program, &spec, &probe_path, &cancelled, None, &failure) {
                Some(installation) => admission_refusal(&installation),
                None => failure.message,
            }
        })?;
        Ok(Self {
            spec,
            program,
            principal_id,
            auth_method,
            probe_path,
            home: std::env::var_os("HOME"),
            user: std::env::var_os("USER"),
        })
    }
    pub fn execution(&self, model: &str, effort: &str) -> Result<WorkerExecutionV1, String> {
        // Bare family aliases are mutable selectors, not exact plan identities.
        if model.len() > 256
            || !model.contains('-')
            || !model.bytes().any(|b| b.is_ascii_digit())
            || model.ends_with("-latest")
            || model.chars().any(char::is_whitespace)
        {
            return Err("Task Model binding requires an explicit versioned model ID".into());
        }
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
    /// that updates itself repoints its launcher at a new version file (ADR-0126).
    fn check_current(&self, deadline: Instant, cancelled: &AtomicBool) -> Result<(), Recheck> {
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
        self.recheck_identity(deadline, cancelled)
    }

    /// The token-free account recheck, telling a CLI that cannot start apart from an account
    /// that changed or could not be proven.
    fn recheck_identity(&self, deadline: Instant, cancelled: &AtomicBool) -> Result<(), Recheck> {
        let (principal, auth_method) = observe_identity(
            &self.program,
            &self.spec,
            &self.probe_path,
            cancelled,
            Some(deadline),
        )
        .map_err(|failure| {
            match cli_cannot_start(
                &self.program,
                &self.spec,
                &self.probe_path,
                cancelled,
                Some(deadline),
                &failure,
            ) {
                Some(installation) => Recheck::CliCannotStart(installation),
                None => Recheck::NotCurrent,
            }
        })?;
        check_task_probe_control(Some(deadline), cancelled).map_err(|_| Recheck::NotCurrent)?;
        if principal != self.principal_id || auth_method != self.auth_method {
            return Err(Recheck::NotCurrent);
        }
        Ok(())
    }
}

/// Why the recheck before a private send refused it.
enum Recheck {
    /// The captured Provider identity is no longer current or could not be verified.
    NotCurrent,
    /// The Provider's CLI cannot start: an environment failure, not the model's or the login's.
    CliCannotStart(CliInstallationFailure),
}

/// Why an account identity probe proved nothing.
struct IdentityFailure {
    message: String,
    /// The CLI gave no answer af recognizes, and af did not give up waiting for one: it may not
    /// start at all. A CLI that answered — logged out, another account — never is.
    cli_silent: bool,
}

impl IdentityFailure {
    fn answered(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cli_silent: false,
        }
    }
}

/// The installation failure behind a silent identity probe, when the CLI's own version check
/// confirms it.
fn cli_cannot_start(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    deadline: Option<Instant>,
    failure: &IdentityFailure,
) -> Option<CliInstallationFailure> {
    if !failure.cli_silent {
        return None;
    }
    diagnose_cli(program, spec, probe_path, cancelled, deadline)
}

fn admission_refusal(failure: &CliInstallationFailure) -> String {
    format!(
        "Task Provider admission refused before any Worker was dispatched: {}",
        failure.message()
    )
}

/// The Attempt diagnostic for a CLI that stopped starting after admission.
fn environment_failure(failure: &CliInstallationFailure) -> String {
    format!(
        "Provider environment failure, not a model or credential failure: {}",
        failure.message()
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
    observe_identity(program, spec, probe_path, cancelled, deadline).map_err(|f| f.message)
}

fn observe_identity(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<(String, String), IdentityFailure> {
    match spec.kind {
        ProviderKind::Claude => {
            let output = run_probe_before(program, spec, probe_path, cancelled, deadline).map_err(
                |message| IdentityFailure {
                    cli_silent: !probe_gave_up(&message),
                    message,
                },
            )?;
            if !output.status.success() {
                return Err(IdentityFailure {
                    message: "Claude account identity probe failed".into(),
                    cli_silent: !status_answered(spec.kind, &output.stdout),
                });
            }
            let status: serde_json::Value = serde_json::from_str(&output.stdout).map_err(|_| {
                IdentityFailure::answered("Claude account identity response is invalid")
            })?;
            let principal = claude_principal(&status).map_err(IdentityFailure::answered)?;
            Ok((principal, status["authMethod"].as_str().unwrap().into()))
        }
        ProviderKind::Codex => {
            let mut answered = false;
            let response = probe_codex_request_observed(
                program,
                spec,
                probe_path,
                cancelled,
                &serde_json::json!({"method":"account/read","id":2,"params":{"refreshToken":false}}),
                deadline,
                &mut answered,
            )
            .map_err(|message| IdentityFailure {
                cli_silent: !answered && !probe_gave_up(&message),
                message,
            })?;
            let principal = codex_principal(&response).map_err(IdentityFailure::answered)?;
            Ok((principal, "chatgpt".into()))
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
            message: Err(reason.into()),
            usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
            usage_observation: None,
            raw_artifact_ids: vec![],
            native_failure: None,
        };
        let refused = || {
            refuse(concat!(
                "Captured Task Provider identity is no longer current or could not be verified ",
                "before invocation"
            ))
        };
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
            match identity.check_current(deadline, cancelled) {
                Ok(()) => {}
                Err(_) if !is_executable(&identity.program) => continue,
                Err(Recheck::CliCannotStart(failure)) => {
                    return refuse(&environment_failure(&failure));
                }
                Err(Recheck::NotCurrent) => return refused(),
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return refused();
            };
            match native.invoke_started(
                cas,
                workdir,
                input.clone(),
                remaining,
                access,
                cancellation,
                environment,
            ) {
                Ok(returned) => return returned,
                Err(Unstarted(_)) if !is_executable(&identity.program) => continue,
                Err(Unstarted(returned)) => return *returned,
            }
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
}

#[cfg(test)]
#[path = "task/currentness_tests.rs"]
mod currentness_tests;
