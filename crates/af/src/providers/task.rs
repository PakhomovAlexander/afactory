//! Fresh token-free account identity proof for Task bindings. Capability probes are dispatched
//! later as paid nodes by the common Task runtime. Credentials and raw account data stay local.
use super::*;
use review_core::task::plan::WorkerExecutionV1;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};

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
        let program = resolve_program(spec.kind.command())
            .ok_or("Task Provider executable is unavailable")?;
        let probe_path = sanitized_path();
        let cancelled = AtomicBool::new(false);
        let (principal_id, auth_method) =
            probe_identity(&program, &spec, &probe_path, &cancelled, None)?;
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
                model.clone(),
                "--effort".into(),
                effort.clone(),
            ],
            ProviderKind::Codex => vec![
                "--model".into(),
                model.clone(),
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
        let inner: Box<dyn WorkerModelAdapter> = match self.spec.kind {
            ProviderKind::Claude => Box::new(
                review_runner_claude::task::ClaudeTaskAdapter::new(&command)?.with_auth(
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
                ),
            ),
            ProviderKind::Codex => Box::new(
                review_runner_codex::task::CodexTaskAdapter::new(&command)?
                    .with_codex_home(auth.into()),
            ),
        };
        Ok(Box::new(CurrentTaskProviderAdapter {
            identity: self.clone(),
            inner,
        }))
    }

    fn check_current(&self, deadline: Instant, cancelled: &AtomicBool) -> Result<(), String> {
        check_task_probe_control(Some(deadline), cancelled)?;
        let current = configured_spec(&self.spec.id)?;
        if current.kind != self.spec.kind
            || current.auth_dir != self.spec.auth_dir
            || current.explicit_selector != self.spec.explicit_selector
            || resolve_program(self.spec.kind.command()).as_ref() != Some(&self.program)
            || !is_executable(&self.program)
            || sanitized_path() != self.probe_path
            || std::env::var_os("HOME") != self.home
            || std::env::var_os("USER") != self.user
        {
            return Err("Task Provider execution or authentication context changed".into());
        }
        let (principal, auth_method) = probe_identity(
            &self.program,
            &self.spec,
            &self.probe_path,
            cancelled,
            Some(deadline),
        )?;
        check_task_probe_control(Some(deadline), cancelled)?;
        if principal != self.principal_id || auth_method != self.auth_method {
            return Err("Task Provider account changed".into());
        }
        Ok(())
    }
}

fn probe_identity(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<(String, String), String> {
    match spec.kind {
        ProviderKind::Claude => {
            let output = run_probe_before(program, spec, probe_path, cancelled, deadline)?;
            if !output.status.success() {
                return Err("Claude account identity probe failed".into());
            }
            let status: serde_json::Value = serde_json::from_str(&output.stdout)
                .map_err(|_| "Claude account identity response is invalid")?;
            let principal = claude_principal(&status)?;
            Ok((principal, status["authMethod"].as_str().unwrap().into()))
        }
        ProviderKind::Codex => {
            let response = probe_codex_request_before(
                program,
                spec,
                probe_path,
                cancelled,
                &serde_json::json!({"method":"account/read","id":2,"params":{"refreshToken":false}}),
                deadline,
            )?;
            Ok((codex_principal(&response)?, "chatgpt".into()))
        }
    }
}

/// A token-free recheck before each private send. The status process and native invocation share
/// the original remaining Attempt wall limit and cancellation. This detects between-node drift;
/// credentials may still change between the check and the native client's auth consumption.
struct CurrentTaskProviderAdapter {
    identity: TaskProviderIdentity,
    inner: Box<dyn WorkerModelAdapter>,
}
impl WorkerModelAdapter for CurrentTaskProviderAdapter {
    fn credential_mode(&self) -> review_core::BrokerCredentialModeV1 {
        self.inner.credential_mode()
    }
    fn provider_kind(&self) -> &'static str {
        self.inner.provider_kind()
    }
    fn model_settings(&self) -> Option<(String, String)> {
        self.inner.model_settings()
    }
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
    ) -> ModelWorkerReturn {
        self.invoke_controlled(cas, workdir, input, timeout, writable, None)
    }
    fn invoke_controlled(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
        cancellation: Option<&AtomicBool>,
    ) -> ModelWorkerReturn {
        self.invoke_controlled_with_environment(
            cas,
            workdir,
            input,
            timeout,
            writable,
            cancellation,
            &[],
        )
    }
    /// The one invocation path. Sandbox-local environment (a carried Build Cache location) is
    /// forwarded to the native client exactly as it arrived; the trait's default would refuse
    /// it, so a wrapper that forgot this method would silently strip a warm layer.
    fn invoke_controlled_with_environment(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        let refused = || ModelWorkerReturn {
            message: Err(concat!(
                "Captured Task Provider identity is no longer current or could not be verified ",
                "before invocation"
            )
            .into()),
            usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
            usage_observation: None,
            raw_artifact_ids: vec![],
        };
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return refused();
        };
        let local = AtomicBool::new(false);
        let cancelled = cancellation.unwrap_or(&local);
        // Raw status/account output and filesystem diagnostics remain local, never Worker evidence.
        if self.identity.check_current(deadline, cancelled).is_err() {
            return refused();
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return refused();
        };
        self.inner.invoke_controlled_with_environment(
            cas,
            workdir,
            input,
            remaining,
            writable,
            cancellation,
            environment,
        )
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
