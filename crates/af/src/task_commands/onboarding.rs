//! Token-free validation of the current committed Task authority format. No Campaign reader.
use super::*;
use af_pipeline_config::task::shared::safe_relative_path;

pub(crate) struct Authority {
    pub(crate) workers: Vec<(String, String)>,
    pub(crate) reviewers: Vec<(String, String, String, String)>,
    pub(crate) gates: Vec<(String, String, Vec<String>)>,
    pub(crate) container: bool,
    pub(crate) clean_rounds: u32,
    pub(crate) max_rounds: u32,
    pub(crate) files: Vec<String>,
}

pub(crate) struct Failure {
    pub(crate) file: Option<PathBuf>,
    pub(crate) line: Option<usize>,
    pub(crate) message: String,
    pub(crate) repin: bool,
}

fn failure(path: &Path, message: impl ToString, line: Option<usize>, repin: bool) -> Failure {
    Failure {
        file: Some(path.into()),
        line,
        message: message
            .to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .into(),
        repin,
    }
}

fn typed<T: serde::de::DeserializeOwned>(path: &Path, text: &str) -> Result<T, Failure> {
    toml::from_str(text).map_err(|error| {
        let line = error.span().map(|span| {
            text[..span.start.min(text.len())]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1
        });
        failure(path, error.message(), line, false)
    })
}

fn under_authority(repo: &Path, relative: &str) -> Result<PathBuf, Failure> {
    if !safe_relative_path(relative) || !relative.starts_with(".af/") {
        return Err(failure(
            &repo.join(".af/task-catalog.toml"),
            "Task authority path must remain under .af/",
            None,
            false,
        ));
    }
    let mut path = repo.to_path_buf();
    for part in Path::new(relative) {
        path.push(part);
        let meta = std::fs::symlink_metadata(&path).map_err(|e| failure(&path, e, None, false))?;
        if meta.file_type().is_symlink() {
            return Err(failure(
                &path,
                "authority must be a regular file or directory, not a symlink",
                None,
                false,
            ));
        }
    }
    Ok(path)
}

fn local_files(repo: &Path) -> Result<BTreeMap<String, Vec<u8>>, Failure> {
    let mut files = BTreeMap::new();
    let mut read = |relative: &str| -> Result<String, Failure> {
        if !safe_relative_path(relative) || !relative.starts_with(".af/") {
            return Err(failure(
                &repo.join(".af/task-catalog.toml"),
                "Task authority path must remain under .af/",
                None,
                false,
            ));
        }
        let path = under_authority(repo, relative)?;
        let text = crate::onboard::read_authority_text(&path)
            .map_err(|e| failure(&path, e, None, false))?;
        files.insert(relative.into(), text.as_bytes().to_vec());
        Ok(text)
    };
    // These are optional in native Task authority. When present, preserve their typed
    // validation and release-pin checks; absence never invents old Review configuration.
    let present = |relative: &str| -> Result<bool, Failure> {
        let path = repo.join(relative);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(failure(&path, e, None, false)),
        }
    };
    if present(".af/af.toml")? {
        let project_path = repo.join(".af/af.toml");
        let project = read(".af/af.toml")?;
        let _: crate::project_policy::ProjectFile = typed(&project_path, &project)?;
        crate::project_policy::ProjectFile::parse(&project)
            .map_err(|e| failure(&project_path, e, None, false))?;
    }
    if present(".af/af.lock")? {
        let lock_path = repo.join(".af/af.lock");
        let lock_text = read(".af/af.lock")?;
        let _: af_pipeline_config::lock::Lockfile = typed(&lock_path, &lock_text)?;
    }
    let path = repo.join(".af/task-catalog.toml");
    let text = read(".af/task-catalog.toml").map_err(|mut e| {
        if !repo.join(".af/task-catalog.toml").exists() { e.message = "No native Task catalog; Campaign configuration is retired. Review a fresh af onboard scaffold in a separate checkout".into(); }
        e
    })?;
    let catalog: TaskCatalog = typed(&path, &text)?;
    if catalog.schema != "af.task-catalog/2" {
        return Err(failure(&path, "Expected af.task-catalog/2", None, false));
    }
    for policy in [
        catalog.code_policy.as_deref(),
        catalog.document_policy.as_deref(),
        catalog.report_policy.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        read(policy)?;
    }
    let bounds = af_pipeline_config::lock::PackageBounds {
        files: 4096,
        bytes: 16 * 1024 * 1024,
        file_bytes: 1024 * 1024,
    };
    let mut collect = |name: &str, relative: &str| -> Result<(), Failure> {
        if !safe_relative_path(relative) || !relative.starts_with(".af/") {
            return Err(failure(
                &path,
                "Task package path must remain under .af/",
                None,
                false,
            ));
        }
        let root = under_authority(repo, relative)?;
        let package =
            af_pipeline_config::lock::package_files(name, &root, bounds).map_err(|e| {
                use af_pipeline_config::lock::LockError;
                let at = match &e {
                    LockError::Io { path, .. } => path.clone(),
                    LockError::TooLarge { at, .. } if safe_relative_path(at) => root.join(at),
                    _ => root.clone(),
                };
                failure(&at, e, None, false)
            })?;
        for (file, bytes) in package {
            let full = format!("{relative}/{file}");
            let manifest_path = repo.join(&full);
            if matches!(file.as_str(), "worker.toml" | "pipeline.toml") {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|e| failure(&manifest_path, e, None, false))?;
                if file == "worker.toml" {
                    let _: TaskWorkerManifest = typed(&manifest_path, text)?;
                } else if let Err(error) = af_pipeline_config::task::parse_task_pipeline(text) {
                    let _: af_contracts::task::pipeline::PipelineDefinition =
                        typed(&manifest_path, text)?;
                    return Err(failure(&manifest_path, error, None, false));
                }
            }
            files.insert(full, bytes);
        }
        if files.len() > 16384 || files.values().map(Vec::len).sum::<usize>() > 64 * 1024 * 1024 {
            return Err(failure(
                &path,
                "Task authority exceeds its captured closure bound",
                None,
                false,
            ));
        }
        Ok(())
    };
    for (name, pin) in &catalog.packages {
        collect(name, &pin.path)?;
    }
    for import in &catalog.imports {
        let parent = Path::new(import)
            .parent()
            .and_then(|p| p.to_str())
            .ok_or_else(|| failure(&path, "Invalid import lock path", None, false))?;
        collect("import", parent)?;
    }
    Ok(files)
}

pub(crate) fn inspect(repo: &Path) -> Result<Authority, Failure> {
    let files = local_files(repo)?;
    inspect_files(&files).map_err(|message| {
        let catalog: Option<TaskCatalog> = files
            .get(".af/task-catalog.toml")
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|t| toml::from_str(t).ok());
        if let Some(catalog) = catalog {
            if let Some((_, pin)) = catalog
                .packages
                .iter()
                .find(|(name, _)| message.contains(&format!("Task package {name} changed")))
            {
                return failure(&repo.join(&pin.path), message, None, true);
            }
        }
        failure(&repo.join(".af/task-catalog.toml"), message, None, false)
    })
}

pub(crate) fn inspect_files(files: &BTreeMap<String, Vec<u8>>) -> Result<Authority, String> {
    let text = |path: &str| -> Result<&str, String> {
        std::str::from_utf8(files.get(path).ok_or_else(|| format!(
            "Missing {path}; retired Campaign authority is not read. Review a native `af onboard` scaffold in a separate checkout and commit its Task catalog"
        ))?).map_err(|e| format!("{path}: {e}"))
    };
    if files.contains_key(".af/af.toml") {
        crate::project_policy::ProjectFile::parse(text(".af/af.toml")?)?;
    }
    if files.contains_key(".af/af.lock") {
        let lock = af_pipeline_config::lock::Lockfile::from_toml(text(".af/af.lock")?)
            .map_err(|e| e.to_string())?;
        crate::project_policy::check_lock_af_version(&lock, ".af/af.lock")?;
    }
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for (path, bytes) in files {
        entries.push(af_source_git::Entry {
            path: path.clone(),
            kind: EntryKind::File,
            content: cas.put(bytes).map_err(|e| e.to_string())?,
            size: bytes.len() as u64,
        });
    }
    let manifest = Manifest::new(entries).map_err(|e| e.to_string())?;
    // This is the actual Task capture/restore path, without admission, planning or execution.
    let catalog: TaskCatalog =
        toml::from_str(text(".af/task-catalog.toml")?).map_err(|e| e.to_string())?;
    let kind = if catalog.review.is_some() {
        "review"
    } else if catalog.code_policy.is_some() {
        "implement"
    } else if catalog.document_policy.is_some() {
        "document"
    } else if catalog.report_policy.is_some() {
        "report"
    } else {
        catalog
            .kinds
            .keys()
            .next()
            .map(String::as_str)
            .ok_or("Task catalog declares no supported Task kind")?
    };
    let (_, authority, compiler) =
        capture_authority(&cas, &manifest, None, kind, Default::default())?;
    compiler.validate_dependency_closure()?;
    let policy: Option<CodeTaskPolicy> = authority
        .code_policy_id
        .as_deref()
        .map(|id| {
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())
        })
        .transpose()?;
    let workers = catalog
        .packages
        .keys()
        .filter_map(|name| {
            compiler.worker(name).map(|worker| {
                (
                    name.clone(),
                    match &worker.runner {
                        TaskWorkerRunner::Command { command } => command.program.clone(),
                        TaskWorkerRunner::Model { provider_kind, .. } => provider_kind.clone(),
                    },
                )
            })
        })
        .collect();
    let mut reviewers = Vec::new();
    for pipeline in compiler.pipelines().values() {
        for (name, slot) in &pipeline.slots {
            let Some(worker) = compiler.worker(&slot.worker) else {
                continue;
            };
            if !worker
                .signature
                .contract
                .outputs
                .values()
                .any(|p| p.artifact_type == af_contracts::contract::REVIEWER_RESULT_V2)
            {
                continue;
            }
            let (runner, model) = match &worker.runner {
                TaskWorkerRunner::Command { command } => {
                    (command.program.clone(), "command".into())
                }
                TaskWorkerRunner::Model {
                    provider_kind,
                    model,
                    effort,
                } => (provider_kind.clone(), format!("{model} ({effort} effort)")),
            };
            let row = (name.clone(), slot.worker.clone(), runner, model);
            if !reviewers.contains(&row) {
                reviewers.push(row);
            }
        }
    }
    let clean_rounds = catalog.review.as_ref().map_or(0, |r| r.clean_rounds);
    let max_rounds = catalog.review.as_ref().map_or(0, |r| r.max_rounds);
    let container = policy.as_ref().is_some_and(|p| p.require_container);
    Ok(Authority {
        workers,
        reviewers,
        gates: policy
            .into_iter()
            .flat_map(|p| p.checks)
            .map(|(name, c)| {
                (
                    name,
                    c.command.program,
                    c.command.args.into_iter().map(|a| a.value).collect(),
                )
            })
            .collect(),
        container,
        clean_rounds,
        max_rounds,
        files: files.keys().cloned().collect(),
    })
}

/// Re-pin only current packages. No reader or rewrite of retired Campaign configuration.
pub(crate) fn refresh(repo: &Path) -> Result<(), String> {
    let mut files = local_files(repo).map_err(|e| e.message)?;
    let path = ".af/task-catalog.toml";
    let mut catalog: TaskCatalog = parse(
        Path::new(path),
        files
            .get(path)
            .ok_or("No native Task catalog; retired Campaign locks cannot be refreshed")?,
    )?;
    let mut changed = false;
    for (name, pin) in &mut catalog.packages {
        let digest = af_pipeline_config::lock::package_digest(name, &repo.join(&pin.path))
            .map_err(|e| e.to_string())?;
        if pin.digest != digest {
            pin.digest = digest;
            changed = true;
        }
    }
    if !changed {
        // A release-pin refresh also comes here. Keep project comments and formatting
        // byte-for-byte when package pins are current, but still validate all authority.
        inspect_files(&files)?;
        return Ok(());
    }
    let text = toml::to_string_pretty(&catalog).map_err(|e| e.to_string())?;
    files.insert(path.into(), text.as_bytes().to_vec());
    inspect_files(&files)?;
    crate::onboard::atomic_replace_authority(&repo.join(path), text.as_bytes())
}
