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
fn safe_installation(program: &Path) -> Result<PathBuf, String> {
    let program = fs::canonicalize(program).map_err(|_| "Pi installation is missing; install Node >=22.19 and official Pi SDK 1.4.0 outside the repository")?;
    let cwd = std::env::current_dir()
        .and_then(fs::canonicalize)
        .map_err(|_| "Cannot resolve current directory")?;
    if program.starts_with(&cwd)
        || program
            .ancestors()
            .any(|p| p.join(".git").exists() || p.join(".af").is_dir())
    {
        return Err("Pi must use an installed SDK/Node outside repository-owned paths".into());
    }
    Ok(program)
}
/// Resolve installed layouts without executing (or interpreting) the Pi launcher.
/// Managed installations select only the supported release, never a path from shell text.
fn discover_sdk() -> Result<PathBuf, String> {
    let unresolved = "Cannot safely resolve Pi SDK; pass --sdk-entrypoint /installed/node_modules/@earendil-works/pi-coding-agent/dist/index.js";
    let launcher = resolve_program("pi").ok_or(unresolved)?;
    discover_sdk_from_launcher(&launcher)
}

fn discover_sdk_from_launcher(launcher: &Path) -> Result<PathBuf, String> {
    let unresolved = "Cannot safely resolve Pi SDK; pass an explicit installed --sdk-entrypoint";
    let launcher = safe_installation(launcher)?;
    let bin = launcher.parent().ok_or(unresolved)?;
    let agent = bin.parent().ok_or(unresolved)?;
    let install = agent.join("install");
    if launcher.file_name().is_some_and(|name| name == "pi")
        && bin.file_name().is_some_and(|name| name == "bin")
        && install.exists()
    {
        let invalid = "Pi managed installation requires a regular current-version containing exactly supported SDK version 1.4.0 and an SDK inside that release";
        if safe_installation(&install)? != install {
            return Err(invalid.into());
        }
        // Bound reads and refuse links/special files before consulting the version.
        let version_file = install.join("current-version");
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(&version_file)
            .map_err(|_| invalid)?;
        if !file.metadata().map_err(|_| invalid)?.is_file() {
            return Err(invalid.into());
        }
        let mut version = String::new();
        file.take(64)
            .read_to_string(&mut version)
            .map_err(|_| invalid)?;
        let version = version.strip_suffix('\n').unwrap_or(&version);
        let version = version.strip_suffix('\r').unwrap_or(version);
        if version != review_runner_pi::SDK_VERSION {
            return Err(invalid.into());
        }
        let release = install.join("releases").join(version);
        let canonical_release = safe_installation(&release)?;
        // A symlinked install/release or package must not redirect discovery elsewhere.
        if canonical_release != release {
            return Err(invalid.into());
        }
        let entry = release.join("node_modules/@earendil-works/pi-coding-agent/dist/index.js");
        let canonical_entry = safe_installation(&entry)?;
        if !canonical_entry.starts_with(&release) || !canonical_entry.is_file() {
            return Err(invalid.into());
        }
        return Ok(canonical_entry);
    }
    // Existing SDK bundle CLI layout: dist/bundle/cli.js -> dist/index.js.
    Ok(agent.join("index.js"))
}
pub(super) fn node() -> Result<PathBuf, String> {
    safe_installation(&resolve_program("node").ok_or("Pi requires Node >=22.19 on PATH")?)
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
    safe_installation(&config.sdk_entrypoint)?;
    let auth = spec
        .auth_dir
        .as_deref()
        .ok_or("Pi requires an explicit auth directory")?;
    validate_private_auth_directory(auth).map_err(|_| "Pi auth directory must be an existing private, owned directory without unsafe links or permissions")?;
    if !auth.join("auth.json").is_file() {
        return Err("Pi requires an existing supported stored OAuth login; log in with Pi at a private terminal".into());
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
        Ok((_, ready)) => {
            status.status = "authenticated".into();
            status.auth_type = "OAuth".into();
            status.detail = format!(
                "Pi SDK 1.4.0; {} stored OAuth; {}; confined file tools; quota unsupported",
                ready.backend,
                if ready.principal.is_some() {
                    "account principal checked"
                } else {
                    "account identity unavailable; auth context only; account substitution cannot be detected"
                }
            );
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
// Every internal refusal goes through setup's single emitter. These errors contain only
// AF-authored text; shared filesystem/registry validators may include arbitrary input bytes.
fn setup_outcome(
    id: &str,
    backend: Option<&str>,
    auth: Option<&Path>,
    sdk: Option<&Path>,
    login: bool,
) -> Result<(i32, &'static str, String), String> {
    validate_explicit_id(id).map_err(|_| "Pi setup requires a valid, non-reserved Provider ID")?;
    availability::check_id(id).map_err(|_| "Pi Provider is disabled or its availability state is invalid; inspect af provider status")?;
    if login {
        return Err("Pi login is human-driven: register an existing supported OAuth profile with --auth-dir".into());
    }
    let backend = backend.ok_or("Pi setup requires --backend openai-codex or kimi-coding")?;
    if !matches!(backend, "openai-codex" | "kimi-coding") {
        return Err("Pi supports openai-codex and kimi-coding with stored OAuth".into());
    }
    let auth = auth
        .ok_or("Pi setup requires an absolute --auth-dir holding an existing stored OAuth login")?;
    if !auth.is_absolute() {
        return Err("Pi auth directory must be absolute".into());
    }
    validate_utf8_destination(auth)
        .map_err(|_| "Pi auth directory must be a valid UTF-8 destination")?;
    validate_private_auth_directory(auth).map_err(|_| "Pi auth directory must be an existing private, owned directory without unsafe links or permissions")?;
    let auth = fs::canonicalize(auth).map_err(|_| "Pi auth directory is missing")?;
    let resolved = match sdk {
        Some(path) => path.to_path_buf(),
        None => discover_sdk()?,
    };
    let mut config = PiConfig {
        backend: backend.into(),
        sdk_entrypoint: resolved,
    };
    config.validate()?;
    config.sdk_entrypoint = safe_installation(&config.sdk_entrypoint)?;
    let registry = registry_path()
        .map_err(|_| "Cannot resolve Pi Provider registry")?
        .ok_or("Cannot locate Provider registry")?;
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
    // Probe diagnostics are a closed AF-authored vocabulary; no native stderr escapes.
    probe(&spec, &cancelled, PROBE_TIMEOUT)?;
    let _lock = registry_lock(&registry).map_err(|_| "Cannot lock Pi Provider registry")?;
    let identity_detail = if backend == "kimi-coding" {
        "account identity unavailable; auth context only; account substitution cannot be detected"
    } else {
        "account principal checked"
    };
    match add_to_registry_with_pi(&registry, id, ProviderKind::Pi, &auth, true, Some(&config))
        .map_err(|_| "Cannot update Pi Provider registry; inspect its configuration and permissions")?
    {
        RegistryAdd::Added(_) => Ok((EXIT_OK, "registered", format!("Pi SDK 1.4.0 {backend} stored OAuth registered; {identity_detail}"))),
        RegistryAdd::AlreadyPresent => Ok((EXIT_OK, "already_registered", format!("Pi registration already present; {identity_detail}"))),
        RegistryAdd::Conflict(_) => Ok((EXIT_REGISTRY_CONFLICT, "registry_conflict", "Pi Provider ID or auth directory conflicts with an existing registration or registry limit".into())),
    }
}

pub fn setup(
    id: &str,
    backend: Option<&str>,
    auth: Option<&Path>,
    sdk: Option<&Path>,
    login: bool,
    json: bool,
) -> Result<i32, String> {
    let (code, state, detail) = setup_outcome(id, backend, auth, sdk, login)
        .unwrap_or_else(|detail| (EXIT_HUMAN_ACTION_REQUIRED, "human_action_required", detail));
    // Never fabricate a supported identity for an invalid request, or echo raw invalid input.
    let id = validate_explicit_id(id).is_ok().then_some(id);
    let backend = backend.filter(|value| matches!(*value, "openai-codex" | "kimi-coding"));
    if json {
        println!(
            "{}",
            serde_json::json!({"schema":"af/pi-provider-setup@3","id":id,"kind":"pi","backend":backend,"outcome":state,"detail":detail})
        );
    } else {
        println!("provider {}: {detail}", id.unwrap_or("pi (invalid ID)"));
    }
    if code != EXIT_OK {
        eprintln!("af provider setup: {detail} ({state})");
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_setup_login_refusal_precedes_discovery_and_authentication() {
        assert_eq!(
            setup_outcome("pi-one", None, None, None, true).unwrap_err(),
            "Pi login is human-driven: register an existing supported OAuth profile with --auth-dir"
        );
    }

    #[test]
    fn managed_sdk_discovery_rejects_every_malformed_version_without_executing_launcher() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let agent = root.join("agent");
        let launcher = agent.join("bin/pi");
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        let marker = root.join("launcher-executed");
        fs::write(
            &launcher,
            format!("#!/bin/sh\n: > '{}'\nexit 99\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        let release = agent.join("install/releases/1.4.0");
        let sdk = release.join("node_modules/@earendil-works/pi-coding-agent/dist/index.js");
        fs::create_dir_all(sdk.parent().unwrap()).unwrap();
        fs::write(&sdk, "must not execute SDK during discovery").unwrap();
        let current = agent.join("install/current-version");
        for bad in [
            "",
            "..",
            "../1.4.0",
            "/1.4.0",
            "1.5.0",
            "1.4.0/../../outside",
            "1.4.0\nextra",
            " 1.4.0",
            "1.4.0\0",
            &"x".repeat(4096),
        ] {
            fs::write(&current, bad).unwrap();
            assert!(
                discover_sdk_from_launcher(&launcher).is_err(),
                "accepted {bad:?}"
            );
            assert!(!marker.exists());
        }
        fs::write(&current, [0xff, 0xfe]).unwrap();
        assert!(discover_sdk_from_launcher(&launcher).is_err());
        for version in ["1.4.0", "1.4.0\n", "1.4.0\r\n"] {
            fs::write(&current, version).unwrap();
            assert_eq!(discover_sdk_from_launcher(&launcher).unwrap(), sdk);
        }
        let saved = root.join("saved-version");
        fs::rename(&current, &saved).unwrap();
        std::os::unix::fs::symlink(&saved, &current).unwrap();
        assert!(discover_sdk_from_launcher(&launcher).is_err());
        fs::remove_file(&current).unwrap();
        fs::create_dir(&current).unwrap();
        assert!(discover_sdk_from_launcher(&launcher).is_err());
        fs::remove_dir(&current).unwrap();
        fs::rename(&saved, &current).unwrap();
        let escaped = root.join("escaped-release");
        fs::rename(&release, &escaped).unwrap();
        std::os::unix::fs::symlink(&escaped, &release).unwrap();
        assert!(discover_sdk_from_launcher(&launcher).is_err());
        assert!(!marker.exists());
    }
}
