//! Pi's token-free setup/status/catalog boundary. Raw credentials never enter this process.
use super::*;
use review_runner_pi::{PiConfig, Ready};

pub(super) fn registry_config(
    kind: ProviderKind,
    table: &toml::Table,
) -> Result<Option<PiConfig>, String> {
    if kind != ProviderKind::Pi {
        if table.contains_key("backend") || table.contains_key("sdk_entrypoint") {
            return Err("Only Pi registrations carry backend/SDK configuration".into());
        }
        return Ok(None);
    }
    let config = PiConfig {
        backend: table
            .get("backend")
            .and_then(toml::Value::as_str)
            .ok_or("Pi requires explicit backend")?
            .into(),
        sdk_entrypoint: table
            .get("sdk_entrypoint")
            .and_then(toml::Value::as_str)
            .ok_or("Pi requires an installed SDK entrypoint")?
            .into(),
    };
    config.validate()?;
    Ok(Some(config))
}
fn safe_installation(program: &Path, module: bool) -> Result<PathBuf, String> {
    let program = fs::canonicalize(program).map_err(|_| "Pi installation is missing; install Node >=22.19 and official Pi SDK 1.4.0 outside the repository")?;
    let cwd = std::env::current_dir()
        .and_then(fs::canonicalize)
        .map_err(|_| "Cannot resolve current directory")?;
    if program.starts_with(&cwd)
        || program
            .ancestors()
            .any(|p| (module && p.join(".git").exists()) || p.join(".af").is_dir())
    {
        return Err("Pi must use an installed SDK/Node outside repository-owned paths".into());
    }
    Ok(program)
}
pub(super) fn node() -> Result<PathBuf, String> {
    safe_installation(
        &resolve_program("node").ok_or("Pi requires Node >=22.19 on PATH")?,
        false,
    )
}
pub(super) fn probe(
    spec: &ProviderSpec,
    cancelled: &AtomicBool,
    timeout: Duration,
) -> Result<(PathBuf, Ready), String> {
    let config = spec
        .pi
        .as_ref()
        .ok_or("Pi registration lacks backend/SDK authority")?;
    safe_installation(&config.sdk_entrypoint, true)?;
    let auth = spec
        .auth_dir
        .as_deref()
        .ok_or("Pi requires an explicit auth directory")?;
    validate_private_auth_directory(auth)?;
    if !auth.join("auth.json").is_file() {
        return Err("Pi requires an existing stored openai-codex OAuth login; log in with Pi at a private terminal".into());
    }
    let node = node()?;
    let ready = review_runner_pi::probe(&node, config, auth, timeout, Some(cancelled))?;
    Ok((node, ready))
}
pub(super) fn status(
    spec: &ProviderSpec,
    cancelled: &AtomicBool,
    usage: UsageProbe,
) -> (ProviderStatus, String) {
    match availability::until(&spec.id) {
        Ok(Some(expiry)) => {
            let mut row = unprobed_status(spec);
            row.status = "disabled".into();
            row.disabled_until = Some(expiry);
            row.detail = expiry.diagnostic(&spec.id);
            row.usage = UsageState::NotApplicable;
            return (row, String::new());
        }
        Err(error) => return (unavailable_status(spec, &error), String::new()),
        Ok(None) => {}
    }
    let mut status = unavailable_status(
        spec,
        "Pi SDK/authentication check could not establish identity",
    );
    match probe(spec, cancelled, PROBE_TIMEOUT) {
        Ok(_) => {
            status.status = "authenticated".into();
            status.auth_type = "OAuth".into();
            status.detail =
                "Pi SDK 1.4.0; openai-codex stored OAuth; confined file tools; quota unsupported"
                    .into();
            status.usage = if usage == UsageProbe::Probe {
                UsageState::Unsupported
            } else {
                UsageState::NotRequested
            };
        }
        Err(error) => {
            status.detail = error;
        }
    }
    (status, String::new())
}
pub fn setup(
    id: &str,
    backend: Option<&str>,
    auth: Option<&Path>,
    sdk: Option<&Path>,
    login: bool,
    json: bool,
) -> Result<i32, String> {
    validate_explicit_id(id)?;
    availability::check_id(id)?;
    if login {
        return Err("Pi login is human-driven: register an existing openai-codex OAuth profile with --auth-dir".into());
    }
    let backend = backend.ok_or("Pi setup requires --backend openai-codex")?;
    let auth = auth
        .ok_or("Pi setup requires an absolute --auth-dir holding an existing stored OAuth login")?;
    if !auth.is_absolute() {
        return Err("Pi auth directory must be absolute".into());
    }
    validate_utf8_destination(auth)?;
    validate_private_auth_directory(auth)?;
    let auth = fs::canonicalize(auth).map_err(|_| "Pi auth directory is missing")?;
    let resolved = sdk.map(PathBuf::from).or_else(|| resolve_program("pi").and_then(|p| fs::canonicalize(p).ok()).and_then(|p| p.parent()?.parent().map(|p| p.join("index.js"))))
        .ok_or("Cannot safely resolve Pi SDK; pass --sdk-entrypoint /installed/node_modules/@earendil-works/pi-coding-agent/dist/index.js")?;
    let config = PiConfig {
        backend: backend.into(),
        sdk_entrypoint: safe_installation(&resolved, true)?,
    };
    config.validate()?;
    let registry = registry_path()?.ok_or("Cannot locate Provider registry")?;
    let spec = ProviderSpec {
        id: id.into(),
        kind: ProviderKind::Pi,
        auth_dir: Some(auth.clone()),
        pi: Some(config.clone()),
        explicit_selector: true,
        registry_declared: false,
        source: registry.display().to_string(),
    };
    let cancelled = AtomicBool::new(false);
    let outcome = probe(&spec, &cancelled, PROBE_TIMEOUT);
    let (code, state, detail) = match outcome {
        Err(error) => (EXIT_HUMAN_ACTION_REQUIRED, "human_action_required", error),
        Ok(_) => {
            let _lock = registry_lock(&registry)?;
            match add_to_registry_with_pi(
                &registry,
                id,
                ProviderKind::Pi,
                &auth,
                true,
                Some(&config),
            )? {
                RegistryAdd::Added(_) => (
                    EXIT_OK,
                    "registered",
                    "Pi SDK 1.4.0 openai-codex OAuth registered".into(),
                ),
                RegistryAdd::AlreadyPresent => (
                    EXIT_OK,
                    "already_registered",
                    "Pi registration already present".into(),
                ),
                RegistryAdd::Conflict(error) => {
                    (EXIT_REGISTRY_CONFLICT, "registry_conflict", error)
                }
            }
        }
    };
    if json {
        println!(
            "{}",
            serde_json::json!({"schema":"af/pi-provider-setup@1","id":id,"kind":"pi","backend":backend,"outcome":state,"detail":detail})
        );
    } else {
        println!("provider {id}: {detail}");
    }
    Ok(code)
}
