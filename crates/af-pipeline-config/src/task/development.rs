//! The optional pinned Worker development companion, and machine-only binding admission.
use af_contracts::{contract::WARM_SET_V3, task::pipeline::PortAffinityV1};
use af_graph::task::OperatorSignature;
use std::collections::BTreeMap;
use std::path::Path;

use af_contracts::task::development::{
    DEVELOPMENT_FILE, DEVELOPMENT_WARM_PORT, WorkerDevelopmentBindingV3, WorkerDevelopmentV3,
};

pub fn parse_development(
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Option<WorkerDevelopmentV3>, String> {
    let Some(bytes) = files.get(DEVELOPMENT_FILE) else {
        return Ok(None);
    };
    let request: WorkerDevelopmentV3 =
        toml::from_str(std::str::from_utf8(bytes).map_err(|error| error.to_string())?)
            .map_err(|error| crate::excerpt::bounded(&error.to_string()))?;
    request.validate_container()?;
    Ok(Some(request))
}

/// The operator, never a package path, selects this file. It grants no credentials or host mappings:
/// approved dependency Manifests and cache ceilings are explicit machine authority. No ambient fallback to a development host exists.
pub fn read_binding(path: &Path) -> Result<WorkerDevelopmentBindingV3, String> {
    if !path.is_absolute() {
        return Err("Worker development binding needs an absolute machine path".into());
    }
    use nix::fcntl::{OFlag, open, openat};
    use nix::sys::stat::Mode;
    use std::io::Read;
    let mut directory = open(
        "/",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| error.to_string())?;
    let components: Vec<_> = path.components().skip(1).collect();
    let (file, parents) = components
        .split_last()
        .ok_or("Development binding has no filename")?;
    for component in parents {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err("Development binding needs canonical path components".into());
        }
        directory = openat(
            &directory,
            component.as_os_str(),
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| error.to_string())?;
    }
    if !matches!(file, std::path::Component::Normal(_)) {
        return Err("Development binding needs a regular filename".into());
    }
    let descriptor = openat(
        &directory,
        file.as_os_str(),
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| error.to_string())?;
    let file = std::fs::File::from(descriptor);
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("Development binding must be a regular file".into());
    }
    let mut text = String::new();
    file.take(64 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() > 64 * 1024 {
        return Err("Development binding exceeds 64 KiB".into());
    }
    let binding: WorkerDevelopmentBindingV3 = toml::from_str(&text).map_err(|_| {
        "Worker development binding is invalid or has unsupported fields".to_owned()
    })?;
    binding.validate()?;
    Ok(binding)
}

/// Rechecked during planning and before each Attempt's preparation. Exact binding identity
/// lives in the pinned package, so a changed machine file cannot widen a resumed Task.
pub fn resolve_binding(
    request: &WorkerDevelopmentV3,
) -> Result<WorkerDevelopmentBindingV3, String> {
    let path = std::env::var_os("AF_WORKER_DEVELOPMENT_BINDING_FILE").ok_or(
        "Worker development requires AF_WORKER_DEVELOPMENT_BINDING_FILE from the operator",
    )?;
    let binding = read_binding(Path::new(&path))?;
    binding.admit(request)?;
    let identity = af_store::canonical::content_id(
        &serde_json::to_value(&binding).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if identity != request.binding_identity {
        return Err(
            "Worker development machine binding differs from the exact package request".into(),
        );
    }
    Ok(binding)
}

pub fn validate_ports(
    request: &WorkerDevelopmentV3,
    signature: &OperatorSignature,
) -> Result<(), String> {
    if request.caches.is_empty() {
        return Ok(());
    }
    let warm = signature.contract.inputs.get(DEVELOPMENT_WARM_PORT).ok_or("Development caches require an optional unbound development_warm input of review.kernel/WarmSet@3")?;
    if !warm.optional
        || warm.artifact_type != WARM_SET_V3
        || warm.cardinality != af_contracts::PortCardinality::One
        || !matches!(warm.affinity, PortAffinityV1::Unbound {})
    {
        return Err("Development Warm Set must be one optional unbound kernel input".into());
    }
    for name in request
        .caches
        .values()
        .filter_map(|policy| policy.output.as_ref())
    {
        if signature
            .contract
            .outputs
            .get(name)
            .is_some_and(|port| !port.covers.is_empty())
            || signature
                .evidence
                .get(name)
                .is_some_and(|policies| !policies.is_empty())
            || signature
                .retains
                .get(name)
                .is_some_and(|ports| !ports.is_empty())
            || signature.outcome_port.as_ref() == Some(name)
            || signature.worker_output_type.as_deref()
                == Some(af_contracts::contract::BUILD_CACHE_V2)
        {
            return Err(
                "Worker caches cannot claim acceptance, branching or receipt authority".into(),
            );
        }
    }
    for policy in request.caches.values() {
        for (name, ports) in policy
            .input
            .iter()
            .map(|name| (name, &signature.contract.inputs))
            .chain(
                policy
                    .output
                    .iter()
                    .map(|name| (name, &signature.contract.outputs)),
            )
        {
            let port = ports
                .get(name)
                .ok_or("Cache policy names an undeclared port")?;
            if port.artifact_type != af_contracts::contract::BUILD_CACHE_V2
                || !port.optional
                || port.cardinality != af_contracts::PortCardinality::One
                || !matches!(port.affinity, PortAffinityV1::Unbound {})
            {
                return Err("Cache ports must be optional unbound BuildCache@2 values".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use af_contracts::task::development::*;

    fn binding() -> WorkerDevelopmentBindingV3 {
        let identity = format!("sha256:{}", "a".repeat(64));
        WorkerDevelopmentBindingV3 {
            dependency_sources: Default::default(),
            cache_limits: None,
            schema: "af.worker-development-binding/3".into(),
            storage: af_contracts::task::development::WorkerStorageBackendV1::TmpfsVolume,
            runtime: "/usr/bin/docker".into(),
            image: format!("fixture.invalid/toolchain@{identity}"),
            checksum_program: "/usr/bin/sha256sum".into(),
            probe_shell: "/bin/sh".into(),
            tools: BTreeMap::from([(
                "sh".into(),
                DevelopmentToolV1 {
                    program: "/bin/sh".into(),
                    identity,
                },
            )]),
            limits: DevelopmentLimitsV1 {
                wall_ms: 1000,
                output_bytes: 4096,
                cpu_millicores: Some(1000),
                memory_bytes: Some(67108864),
                disk_bytes: Some(268435456),
                processes: Some(32),
            },
        }
    }

    #[test]
    fn machine_development_binding_reads_regular_files_and_refuses_symlink_leaves_and_ancestors() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let directory = root.join("authority");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("binding.toml");
        let expected = binding();
        std::fs::write(&path, toml::to_string(&expected).unwrap()).unwrap();
        assert_eq!(read_binding(&path).unwrap(), expected);
        let leaf = root.join("alias.toml");
        std::os::unix::fs::symlink(&path, &leaf).unwrap();
        assert!(read_binding(&leaf).is_err());
        let parent = root.join("alias");
        std::os::unix::fs::symlink(&directory, &parent).unwrap();
        assert!(read_binding(&parent.join("binding.toml")).is_err());
        assert!(read_binding(Path::new("relative-binding.toml")).is_err());
    }

    #[test]
    fn pinned_worker_companion_is_optional_but_a_present_unsupported_contract_is_never_ignored() {
        let mut files = BTreeMap::new();
        assert!(parse_development(&files).unwrap().is_none());
        for source in [
            "schema = 'af.worker-development/0'",
            "schema = 'af.worker-development/3'\nhost_cache = '/host/cache'",
        ] {
            files.insert(DEVELOPMENT_FILE.into(), source.as_bytes().to_vec());
            assert!(parse_development(&files).is_err());
        }
    }
}
