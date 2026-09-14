//! Explicit Git sync is separate from Task admission. Only sync may resolve mutable Git refs;
//! normal planning/restoration consumes the committed import lock and vendored package bytes.
use super::*;
use review_config::task::shared::*;
use std::io::Write;

pub(super) fn restore_imports(
    cas: &Cas,
    manifest: &Manifest,
    catalog: &mut TaskCatalog,
) -> Result<BTreeSet<String>, String> {
    if catalog.imports.len() > 32 {
        return Err("Project catalog imports exceed the captured bound".into());
    }
    let mut locks = BTreeSet::new();
    for path in &catalog.imports {
        if !safe_relative_path(path) {
            return Err("Catalog import path is unsafe".into());
        }
        let bytes = captured_file(cas, manifest, path)?;
        let lock: TaskCatalogImportLock = parse(Path::new(path), &bytes)?;
        lock.validate()?;
        let parent = Path::new(path)
            .parent()
            .and_then(|p| p.to_str())
            .ok_or("Import lock has invalid parent")?;
        let relative = |suffix: &str| {
            if parent.is_empty() {
                suffix.to_string()
            } else {
                format!("{parent}/{suffix}")
            }
        };
        for (file, digest) in &lock.catalogs {
            let bytes = captured_file(cas, manifest, &relative(&format!("catalogs/{file}")))?;
            if review_store::canonical::blob_content_id(&bytes) != *digest {
                return Err("Imported catalog provenance changed".into());
            }
        }
        for (name, mut pin) in lock.packages {
            if catalog.packages.contains_key(&name) {
                return Err(format!(
                    "Imported package {name} shadows another catalog entry"
                ));
            }
            if catalog.packages.len() >= 128 {
                return Err("Combined catalog package bound exceeded".into());
            }
            pin.path = relative(&pin.path);
            catalog.packages.insert(name, pin);
        }
        locks.insert(cas.put(&bytes).map_err(|e| e.to_string())?);
    }
    Ok(locks)
}

struct SyncCapture<'a> {
    repo: &'a Repo,
    commit: String,
    compiler: TaskPlanCompiler,
    cas: &'a Cas,
    catalogs: BTreeMap<String, String>,
    stack: BTreeSet<String>,
    packages: BTreeMap<String, TaskPackagePin>,
    files: BTreeMap<String, Vec<u8>>,
    total: usize,
}

impl SyncCapture<'_> {
    fn blob(&mut self, oid: &str) -> Result<Vec<u8>, String> {
        let size: usize = self
            .repo
            .line(&["cat-file", "-s", oid])
            .map_err(|e| e.to_string())?
            .parse()
            .map_err(|_| "Git object has invalid size")?;
        if size > 16 * 1024 * 1024 || self.total.saturating_add(size) > 64 * 1024 * 1024 {
            return Err("Shared catalog exceeds captured byte limits".into());
        }
        let bytes = self
            .repo
            .bytes(&["cat-file", "blob", oid])
            .map_err(|e| e.to_string())?;
        if bytes.len() != size {
            return Err("Git object changed its declared size".into());
        }
        self.total += size;
        Ok(bytes)
    }
    fn file(&mut self, path: &str) -> Result<Vec<u8>, String> {
        if !safe_relative_path(path) {
            return Err("Catalog paths must be safe repository-relative files".into());
        }
        let listing = self
            .repo
            .bytes(&["ls-tree", "-z", &self.commit, "--", path])
            .map_err(|e| e.to_string())?;
        let records = entries(&listing)?;
        let [(found, oid)] = records.as_slice() else {
            return Err(format!("Catalog file {path} is missing or ambiguous"));
        };
        if found != path {
            return Err("Git catalog path does not match the requested file".into());
        }
        self.blob(oid)
    }
    fn visit(&mut self, path: &str, depth: usize) -> Result<(), String> {
        if self.stack.contains(path) {
            return Err("Shared catalog imports form a cycle".into());
        }
        if self.catalogs.contains_key(path) {
            return Ok(());
        }
        if depth >= 4 || self.catalogs.len() >= 32 {
            return Err("Shared catalog import depth or count exceeded".into());
        }
        self.stack.insert(path.into());
        let bytes = self.file(path)?;
        let catalog: SharedTaskCatalog = parse(Path::new(path), &bytes)?;
        catalog.validate()?;
        let resolve = |relative: &str| -> Result<String, String> {
            let path = if catalog.path_base == CatalogPathBase::Manifest {
                Path::new(path)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(relative)
            } else {
                PathBuf::from(relative)
            };
            let text = path.to_str().ok_or("Catalog relative path is not UTF-8")?;
            if !safe_relative_path(text) {
                return Err("Unsafe catalog-relative path".into());
            }
            Ok(text.into())
        };
        self.catalogs.insert(
            path.into(),
            review_store::canonical::blob_content_id(&bytes),
        );
        self.files.insert(format!("catalogs/{path}"), bytes);
        for import in &catalog.imports {
            self.visit(&resolve(import)?, depth + 1)?;
        }
        for (name, pin) in &catalog.packages {
            let pin = TaskPackagePin {
                path: resolve(&pin.path)?,
                ..pin.clone()
            };
            if self.packages.contains_key(name) || self.packages.len() >= 128 {
                return Err(format!(
                    "Ambiguous shared package {name} or catalog package bound exceeded"
                ));
            }
            let listing = self
                .repo
                .bytes(&[
                    "ls-tree",
                    "-r",
                    "-z",
                    "--full-tree",
                    &self.commit,
                    "--",
                    &pin.path,
                ])
                .map_err(|e| e.to_string())?;
            let mut source = BTreeMap::new();
            for (path, oid) in entries(&listing)? {
                if self.files.len() + source.len() >= 4096 {
                    return Err("Shared catalog file count exceeded".into());
                }
                source.insert(path, self.blob(&oid)?);
            }
            self.compiler
                .capture_package(self.cas, name, &pin, &source)?;
            if let Some(worker) = self.compiler.worker(name) {
                use review_pipeline::task::host::TaskEnvironment;
                let environment = SnapshotTaskEnvironment {
                    policy: review_sandbox::Policy::trusted_local(),
                };
                self.compiler.worker_contract(
                    self.cas,
                    name,
                    &environment.kernel_outputs(&worker.signature),
                )?;
            }
            let relative = package_directory(name);
            let prefix = format!("{}/", pin.path);
            for (path, bytes) in source {
                let suffix = path
                    .strip_prefix(&prefix)
                    .ok_or("Package path escaped its declared root")?;
                self.files.insert(format!("{relative}/{suffix}"), bytes);
            }
            self.packages.insert(
                name.clone(),
                TaskPackagePin {
                    path: relative,
                    ..pin.clone()
                },
            );
        }
        self.stack.remove(path);
        Ok(())
    }
}

fn entries(listing: &[u8]) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();
    for record in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if entries.len() >= 4096 {
            return Err("Git catalog listing exceeds file bound".into());
        }
        let text = std::str::from_utf8(record).map_err(|_| "Catalog paths must be UTF-8")?;
        let (meta, path) = text
            .split_once('\t')
            .ok_or("Malformed Git catalog listing")?;
        let fields: Vec<_> = meta.split(' ').collect();
        if fields.len() != 3
            || !matches!(fields[0], "100644" | "100755")
            || fields[1] != "blob"
            || !safe_relative_path(path)
        {
            return Err(
                "Catalog capture permits regular files only; symlinks and submodules are refused"
                    .into(),
            );
        }
        entries.push((path.into(), fields[2].into()));
    }
    Ok(entries)
}

pub(crate) fn sync(
    source: &str,
    revision: &str,
    manifest: &str,
    project: &Path,
    destination: &str,
    json_output: bool,
) -> Result<(), String> {
    if revision.is_empty()
        || revision.len() > 1024
        || revision.starts_with('-')
        || revision.chars().any(char::is_control)
        || !safe_relative_path(destination)
    {
        return Err("Catalog sync needs an explicit revision and safe absent destination".into());
    }
    let destination_path = absent_destination(project, destination)?;
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let git_home = temporary.path().join("home");
    std::fs::create_dir(&git_home).map_err(|e| e.to_string())?;
    let (repository, selector) = if source.starts_with("https://") {
        if revision.contains("..")
            || !revision
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
        {
            return Err(
                "Remote sync requires one branch, tag or commit, not a fetch refspec".into(),
            );
        }
        let rest = source.trim_start_matches("https://");
        if !rest.contains('/')
            || source
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '@' | '?' | '#'))
        {
            return Err(
                "HTTPS catalog source must not contain credentials, query tokens or controls"
                    .into(),
            );
        }
        let bare = temporary.path().join("source.git");
        transport(
            &git_home,
            temporary.path(),
            &[
                "init",
                "--bare",
                bare.to_str().ok_or("Temporary Git path is not UTF-8")?,
            ],
        )?;
        transport(
            &git_home,
            &bare,
            &["fetch", "--depth=1", "--no-tags", "--", source, revision],
        )?;
        (bare, "FETCH_HEAD".to_string())
    } else {
        (
            std::fs::canonicalize(source).map_err(|e| {
                format!("Catalog source must be a local Git checkout or HTTPS URL: {e}")
            })?,
            revision.into(),
        )
    };
    let repo = Repo::open(&repository, &git_home);
    let commit = repo.rev_parse(&selector).map_err(|e| e.to_string())?;
    let cas = Cas::open(temporary.path().join("cas")).map_err(|e| e.to_string())?;
    let policy = cas
        .put(b"catalog-contract-validation@1")
        .map_err(|e| e.to_string())?;
    let compiler = TaskPlanCompiler::new(
        policy.clone(),
        policy,
        BTreeMap::new(),
        BTreeMap::new(),
        IndependencePolicyV1::default(),
    )?;
    let mut captured = SyncCapture {
        repo: &repo,
        commit: commit.clone(),
        compiler,
        cas: &cas,
        catalogs: BTreeMap::new(),
        stack: BTreeSet::new(),
        packages: BTreeMap::new(),
        files: BTreeMap::new(),
        total: 0,
    };
    captured.visit(manifest, 0)?;
    captured.compiler.validate_dependency_closure()?;
    let lock = TaskCatalogImportLock {
        schema: "af.task-catalog-import/1".into(),
        source: CatalogSourceLock {
            source_id: review_store::canonical::blob_content_id(source.as_bytes()),
            requested_revision: revision.into(),
            commit,
        },
        catalogs: captured.catalogs,
        packages: captured.packages,
    };
    lock.validate()?;
    captured.files.insert(
        "catalog.lock.json".into(),
        serde_json::to_vec_pretty(&lock).map_err(|e| e.to_string())?,
    );
    publish_absent(&destination_path, &captured.files)?;
    if json_output {
        println!(
            "{}",
            serde_json::json!({"schema":"af.catalog-sync/1", "lock":format!("{destination}/catalog.lock.json"), "source":lock.source, "packages":lock.packages})
        );
    } else {
        println!(
            "Captured {} packages at {destination}/catalog.lock.json\nReview and commit this import before adding it to the project catalog.",
            lock.packages.len()
        );
    }
    Ok(())
}

pub(super) fn absent_destination(project: &Path, destination: &str) -> Result<PathBuf, String> {
    if !safe_relative_path(destination) {
        return Err("Catalog destination must be a safe project-relative path".into());
    }
    let mut current = std::fs::canonicalize(project).map_err(|e| e.to_string())?;
    for component in destination.split('/') {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                return Err("Catalog destination cannot follow a symlink or non-directory".into());
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    if std::fs::symlink_metadata(&current).is_ok() {
        return Err("Catalog destination must be absent".into());
    }
    Ok(current)
}

fn transport(home: &Path, cwd: &Path, args: &[&str]) -> Result<(), String> {
    let mut command = std::process::Command::new("git");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "credential.helper=",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "submodule.recurse=false",
            "-c",
            "http.followRedirects=false",
        ])
        .current_dir(cwd)
        .args(args);
    let output = review_process::run_supervised_with_policy(
        &mut command,
        None,
        std::time::Duration::from_secs(120),
        review_process::ExitPolicy::KillProcessGroup,
    )
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Catalog Git transport failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn test(
    source: &Path,
    revision: &str,
    manifest: &str,
    fixtures: Option<&str>,
    pipeline: Option<&str>,
    worker: Option<&str>,
    json_output: bool,
) -> Result<(), String> {
    if revision.is_empty()
        || revision.len() > 1024
        || revision.starts_with('-')
        || revision.chars().any(char::is_control)
    {
        return Err("Catalog contract test needs an explicit local Git revision".into());
    }
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let home = temporary.path().join("home");
    std::fs::create_dir(&home).map_err(|e| e.to_string())?;
    let source = std::fs::canonicalize(source).map_err(|e| e.to_string())?;
    let repo = Repo::open(&source, &home);
    let commit = repo.rev_parse(revision).map_err(|e| e.to_string())?;
    let cas = Cas::open(temporary.path().join("cas")).map_err(|e| e.to_string())?;
    let policy = cas
        .put(b"catalog-contract-validation@1")
        .map_err(|e| e.to_string())?;
    let compiler = TaskPlanCompiler::new(
        policy.clone(),
        policy,
        BTreeMap::new(),
        BTreeMap::new(),
        IndependencePolicyV1::default(),
    )?;
    let mut captured = SyncCapture {
        repo: &repo,
        commit: commit.clone(),
        compiler,
        cas: &cas,
        catalogs: BTreeMap::new(),
        stack: BTreeSet::new(),
        packages: BTreeMap::new(),
        files: BTreeMap::new(),
        total: 0,
    };
    captured.visit(manifest, 0)?;
    captured.compiler.validate_dependency_closure()?;
    let default = Path::new(manifest)
        .parent()
        .unwrap_or(Path::new(""))
        .join("contracts.json");
    let fixture_path = fixtures.unwrap_or(default.to_str().ok_or("Fixture path is not UTF-8")?);
    let fixtures: review_config::task::catalog::export::CatalogContractFixtures =
        parse(Path::new(fixture_path), &captured.file(fixture_path)?)?;
    captured.compiler.check_contract_fixtures(&fixtures)?;
    if pipeline.is_some_and(|p| !fixtures.pipelines.contains_key(p))
        || worker.is_some_and(|w| !fixtures.workers.contains_key(w))
    {
        return Err(
            "Requested Pipeline or Worker is absent from the exact catalog fixtures".into(),
        );
    }
    let mut prerequisites = Vec::new();
    for name in fixtures
        .workers
        .keys()
        .filter(|name| worker.is_none_or(|w| w == name.as_str()))
    {
        let manifest = captured
            .compiler
            .worker(name)
            .ok_or("Worker disappeared from catalog")?;
        let requirement = match &manifest.runner {
            TaskWorkerRunner::Model {
                provider_kind,
                model,
                effort,
            } => json!({
                "kind":"provider_binding", "provider_kind":provider_kind, "model":model, "effort":effort,
                "status":"requires_local_admission",
            }),
            TaskWorkerRunner::Command { command }
            | TaskWorkerRunner::LegacyTaskCommand { command, .. } => {
                let path = Path::new(&command.program);
                let found = if path.is_absolute() {
                    path.is_file()
                } else if command.program.contains('/') {
                    false
                } else {
                    std::env::var_os("PATH").is_some_and(|paths| {
                        std::env::split_paths(&paths).any(|p| p.join(path).is_file())
                    })
                };
                json!({"kind":"command", "program":command.program,
                    "status":if found {"present_unexecuted"} else {"missing_or_environment_specific"}})
            }
        };
        prerequisites.push(json!({"worker":name, "requirement":requirement}));
    }
    let result = json!({"schema":"af.catalog-contract-test/1", "commit":commit,
        "contract_fixtures":"passed", "pipelines":fixtures.pipelines.keys().collect::<Vec<_>>(),
        "workers":fixtures.workers.keys().collect::<Vec<_>>(), "prerequisites":prerequisites,
        "attempts":0, "business_acceptance":"not_executed"});
    if json_output {
        println!("{result}");
    } else {
        println!(
            "Contract fixtures passed at {commit}: {} Pipelines, {} Workers.\nNo Workers ran; Task acceptance requires execution under project policy.",
            fixtures.pipelines.len(),
            fixtures.workers.len()
        );
        for prerequisite in prerequisites {
            println!("{prerequisite}");
        }
    }
    Ok(())
}

pub(super) fn publish_absent(
    destination: &Path,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or("Catalog destination lacks parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let staging = tempfile::Builder::new()
        .prefix(".af-catalog-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut directories = BTreeSet::from([staging.path().to_path_buf()]);
    for (path, bytes) in files {
        if !safe_relative_path(path) {
            return Err("Exported package path is unsafe".into());
        }
        let target = staging.path().join(path);
        directories.extend(
            target
                .ancestors()
                .skip(1)
                .take_while(|p| p.starts_with(staging.path()))
                .map(Path::to_path_buf),
        );
        std::fs::create_dir_all(target.parent().ok_or("Package file lacks parent")?)
            .map_err(|e| e.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
    }
    for directory in directories.iter().rev() {
        std::fs::File::open(directory)
            .and_then(|d| d.sync_all())
            .map_err(|e| e.to_string())?;
    }
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staging.path(),
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|e| format!("Catalog destination must be absent; nothing was overwritten: {e}"))?;
    std::fs::File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}
