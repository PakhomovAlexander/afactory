//! One local-driver tmpfs volume for every Worker write. Helpers never execute project bytes.
//! The keeper holds the filesystem mounted across seed, workload and read-only export. Its
//! durable intent belongs to CheckRuntime, so the ordinary dead-owner sweep owns recovery.
use super::*;
use af_contracts::task::development::WorkerStorageLeaseV1;
use std::collections::BTreeSet;
use std::io::{Read, Write};

const MARKER: &str = "worker-storage-v1.json";
const SCHEMA: &str = "af.worker-storage/1";
const MAX_ENTRIES: usize = 16_384;

pub(super) fn mount(argv: &mut Vec<String>, volume: &str, part: &str, target: &str, ro: bool) {
    argv.extend([
        "--mount".into(),
        format!(
            "type=volume,src={volume},dst={target},volume-subpath={part},volume-nocopy{}",
            if ro { ",readonly" } else { "" }
        ),
    ]);
}

pub(super) struct Storage {
    runtime: PathBuf,
    home: PathBuf,
    marker: PathBuf,
    pub name: String,
    cleanup_attempted: std::cell::Cell<bool>,
}

impl Storage {
    pub fn create(
        provider: &ContainerProvider,
        source: &Path,
        home: &Path,
        deadline: Instant,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Self, String> {
        let worker = provider.worker.as_ref().ok_or("Missing Worker layout")?;
        let mut names = BTreeSet::new();
        for (_, name, _) in &worker.roots {
            if !["state", "scratch", "build", "cache", "knowledge"].contains(&name.as_str())
                || !names.insert(name)
            {
                return Err("Worker storage requires distinct canonical root names".into());
            }
        }
        for path in std::iter::once(source)
            .chain(std::iter::once(worker.package.as_path()))
            .chain(worker.roots.iter().map(|(path, _, _)| path.as_path()))
            .chain(worker.base.as_deref())
        {
            if !path.is_absolute()
                || path
                    .to_str()
                    .is_none_or(|path| path.contains([':', ',', '\n', '\r']))
            {
                return Err("Worker storage input has an unsafe mount spelling".into());
            }
        }
        if worker.disk_bytes < 4096
            || worker.disk_bytes > 8 * 1024 * 1024 * 1024
            || worker.disk_bytes % 4096 != 0
        {
            return Err("Worker storage requires a finite page-aligned aggregate bound".into());
        }
        let Availability::Usable { runtime } = &provider.availability else {
            return Err("Worker storage requires a usable local runtime".into());
        };
        let parent = home
            .parent()
            .ok_or("Worker storage has no runtime parent")?;
        let identity = tempfile::Builder::new()
            .prefix("af-storage-")
            .tempdir_in(parent)
            .map_err(|error| error.to_string())?;
        let name = identity
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .ok_or("Invalid storage name")?
            .to_owned();
        let marker = parent.join(MARKER);
        // An existing intent belongs to an earlier dispatch. Refuse without touching it.
        let mut intent = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .map_err(|error| error.to_string())?;
        let storage = Self {
            runtime: runtime.clone(),
            home: home.to_owned(),
            marker,
            name,
            cleanup_attempted: std::cell::Cell::new(false),
        };
        // Persist before the first daemon operation. This marker is never mounted in a Worker.
        intent
            .write_all(
                serde_json::to_string(&WorkerStorageLeaseV1 {
                    schema: SCHEMA.into(),
                    runtime: runtime.to_str().ok_or("Non-UTF-8 storage runtime")?.into(),
                    name: storage.name.clone(),
                })
                .map_err(|error| error.to_string())?
                .as_bytes(),
            )
            .map_err(|error| error.to_string())?;
        intent.sync_all().map_err(|error| error.to_string())?;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?;
        storage.run(
            &[
                "volume".into(),
                "create".into(),
                "--driver=local".into(),
                "--label".into(),
                format!("af.worker-storage={}", storage.name),
                "--opt=type=tmpfs".into(),
                "--opt=device=tmpfs".into(),
                format!(
                    "--opt=o=size={},nr_inodes={MAX_ENTRIES},mode=1777",
                    worker.disk_bytes
                ),
                storage.name.clone(),
            ],
            deadline,
            cancellation,
        )?;
        let mut keeper = storage.helper(provider, "keeper", false);
        keeper.push("--detach".into());
        keeper.extend([
            "--entrypoint".into(),
            worker.probe_shell.clone(),
            provider.image.clone(),
            "-c".into(),
            "exec /bin/sleep \"$1\"".into(),
            "af-storage-keeper".into(),
            deadline
                .saturating_duration_since(Instant::now())
                .as_secs()
                .saturating_add(1)
                .to_string(),
        ]);
        storage.run(&keeper, deadline, cancellation)?;

        let mut seed = storage.helper(provider, "seed", false);
        let mut inputs = vec![(source.to_owned(), "source".to_owned())];
        inputs.extend(
            worker
                .roots
                .iter()
                .map(|(host, name, _)| (host.clone(), name.clone())),
        );
        if let Some(base) = &worker.base {
            inputs.push((base.clone(), "base".into()));
        }
        for (path, name) in &inputs {
            seed.extend([
                "--volume".into(),
                format!("{}:/input/{name}:ro", path.display()),
            ]);
        }
        // The host uid reads its private 0700 inputs. Only a root operator needs CHOWN,
        // granted exclusively to this fixed helper on a private volume with read-only inputs.
        // The selected machine storage backend authorizes this operation; Workers get no caps.
        if nix::unistd::getuid().is_root() {
            seed.push("--cap-add=CHOWN".into());
        }
        seed.extend(["--entrypoint".into(), worker.probe_shell.clone(), provider.image.clone(), "-c".into(),
            "set -eu; test \"$(/usr/bin/stat -f -c %T /disk)\" = tmpfs; test \"$(/usr/bin/stat -f -c %S /disk)\" = 4096; test \"$(/usr/bin/stat -f -c %b /disk)\" = \"$1\"; chown=$2; shift 2; for n do /bin/mkdir /disk/\"$n\"; /bin/cp -R /input/\"$n\"/. /disk/\"$n\"/; done; /bin/mkdir /disk/tmp /disk/run /disk/shm /disk/mqueue; /bin/chmod -R u+rwX /disk/*; if test \"$chown\" = yes; then /bin/chown -R 65534:65534 /disk/*; fi".into(),
            "af-storage-seed".into(), (worker.disk_bytes / 4096).to_string(), if nix::unistd::getuid().is_root() { "yes" } else { "no" }.into()]);
        seed.extend(inputs.iter().map(|(_, name)| name.clone()));
        storage.run(&seed, deadline, cancellation)?;
        Ok(storage)
    }

    fn command(&self, argv: &[String]) -> std::process::Command {
        let mut command = std::process::Command::new(&self.runtime);
        command.args(argv);
        private_client(&mut command, &self.home);
        command
    }

    fn run(
        &self,
        argv: &[String],
        deadline: Instant,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        let mut command = self.command(argv);
        let capture = run_supervised_captured_bounded(
            &mut command,
            None,
            remaining(deadline)?,
            ExitPolicy::KillDescendants,
            cancellation,
            16 * 1024,
        );
        if !capture.status.as_ref().is_ok_and(|status| status.success())
            || capture.stderr_held
            || capture.truncation.is_truncated()
        {
            let status = match &capture.status {
                Ok(status) => format!("exit {:?}", status.code()),
                Err(error) => error.to_string(),
            };
            return Err(format!(
                "Worker aggregate storage operation refused ({status}): {}",
                diagnostic(&capture.stderr)
            ));
        }
        Ok(())
    }

    fn helper(&self, provider: &ContainerProvider, purpose: &str, ro: bool) -> Vec<String> {
        let mut argv = vec![
            "run".into(),
            "--rm".into(),
            "--name".into(),
            format!("{}-{purpose}", self.name),
            "--pull=never".into(),
            "--network=none".into(),
            "--read-only".into(),
            "--no-healthcheck".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            "--log-driver=none".into(),
            "--ipc=none".into(),
            format!(
                "--pids-limit={}",
                provider.worker.as_ref().unwrap().processes
            ),
            "--env-file=/dev/null".into(),
            "--user".into(),
            format!("{}:{}", nix::unistd::getuid(), nix::unistd::getgid()),
            "--mount".into(),
            format!(
                "type=volume,src={},dst=/disk,volume-nocopy{}",
                self.name,
                if ro { ",readonly" } else { "" }
            ),
        ];
        if let Some(memory) = &provider.memory {
            argv.extend([
                "--memory".into(),
                memory.clone(),
                "--memory-swap".into(),
                memory.clone(),
            ]);
        }
        if let Some(cpus) = &provider.cpus {
            argv.extend(["--cpus".into(), cpus.clone()]);
        }
        argv
    }

    pub fn export(
        &self,
        provider: &ContainerProvider,
        source: &Path,
        deadline: Instant,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        let worker = provider.worker.as_ref().unwrap();
        let mut outputs = Vec::new();
        if worker.source_writable {
            outputs.push(("source".to_owned(), source.to_owned()));
        }
        outputs.extend(
            worker
                .roots
                .iter()
                .filter(|(_, _, writable)| *writable)
                .map(|(path, name, _)| (name.clone(), path.clone())),
        );
        if outputs.is_empty() {
            return Ok(());
        }
        let stage = tempfile::Builder::new()
            .prefix("storage-export-")
            .tempdir_in(self.home.parent().unwrap())
            .map_err(|error| error.to_string())?;
        let mut argv = self.helper(provider, "export", true);
        // Read as the workload uid, including when the operator is root. GNU tar expands
        // hard links; sparse files' logical size is checked before any host bytes are written.
        argv.extend([
            format!("--user={}", worker_user()),
            "--entrypoint=/bin/tar".into(),
            provider.image.clone(),
            "--format=gnu".into(),
            "--sort=name".into(),
            "--hard-dereference".into(),
            "--numeric-owner".into(),
            "--owner=0".into(),
            "--group=0".into(),
            "-cf".into(),
            "-".into(),
            "-C".into(),
            "/disk".into(),
        ]);
        argv.extend(outputs.iter().map(|(name, _)| name.clone()));
        let mut command = self.command(&argv);
        let allowed = outputs.iter().map(|(name, _)| name.clone()).collect();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let result = af_process::run_supervised_duplex_cancellable(
            &mut command,
            remaining(deadline)?,
            ExitPolicy::KillDescendants,
            cancellation.unwrap_or(&cancel),
            |_| Ok::<_, std::io::Error>(()),
            |reader| extract(reader, stage.path(), &allowed, worker.disk_bytes),
        )
        .map_err(|error| error.to_string())?;
        if !result.status.success() || result.stderr_held || result.truncation.is_truncated() {
            return Err(format!(
                "Bounded Worker storage export failed: {}",
                diagnostic(&result.stderr)
            ));
        }
        result.output.map_err(|error| error.to_string())?;
        // All exported roots were validated in a private stage before replacing any input.
        // No Worker is running, no exported link can redirect this operation, and no partial
        // result can be admitted when a rename fails.
        for (name, path) in outputs {
            if !stage.path().join(&name).is_dir() {
                return Err("Worker export omitted a requested root".into());
            }
            std::fs::remove_dir_all(&path).map_err(|error| error.to_string())?;
            std::fs::rename(stage.path().join(name), &path).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn cleanup(&self) -> Result<(), String> {
        self.cleanup_attempted.set(true);
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        for purpose in ["worker", "seed", "export", "keeper"] {
            let name = format!("{}-{purpose}", self.name);
            let mut command = self.command(&["rm".into(), "--force".into(), name.clone()]);
            let result = run_supervised_captured_bounded(
                &mut command,
                None,
                remaining(deadline)?,
                ExitPolicy::KillDescendants,
                None,
                4096,
            );
            if !result.status.as_ref().is_ok_and(|status| status.success()) {
                self.absent(
                    &[
                        "ps".into(),
                        "--all".into(),
                        "--filter".into(),
                        format!("name=^/{name}$"),
                        "--format={{.Names}}".into(),
                    ],
                    deadline,
                )?;
            }
        }
        let mut command = self.command(&["volume".into(), "rm".into(), self.name.clone()]);
        let result = run_supervised_captured_bounded(
            &mut command,
            None,
            remaining(deadline)?,
            ExitPolicy::KillDescendants,
            None,
            4096,
        );
        if !result.status.as_ref().is_ok_and(|status| status.success()) {
            self.absent(
                &[
                    "volume".into(),
                    "ls".into(),
                    "--filter".into(),
                    format!("name=^{}$", self.name),
                    "--format={{.Name}}".into(),
                ],
                deadline,
            )?;
        }
        std::fs::remove_file(&self.marker).map_err(|error| error.to_string())?;
        Ok(())
    }

    fn absent(&self, argv: &[String], deadline: Instant) -> Result<(), String> {
        let mut command = self.command(argv);
        let capture = run_supervised_captured_bounded(
            &mut command,
            None,
            remaining(deadline)?,
            ExitPolicy::KillDescendants,
            None,
            4096,
        );
        if capture.status.as_ref().is_ok_and(|status| status.success())
            && !capture.stderr_held
            && !capture.truncation.is_truncated()
            && capture.stdout.iter().all(u8::is_ascii_whitespace)
        {
            Ok(())
        } else {
            Err("Worker storage cleanup was not confirmed; runtime preserved".into())
        }
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        if self.marker.exists() && (self.cleanup_attempted.get() || self.cleanup().is_err()) {
            let _ = crate::stale::mark_preserved(self.marker.parent().unwrap());
        }
    }
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|time| !time.is_zero())
        .ok_or("Worker aggregate storage deadline expired".into())
}

/// Called only for a dead runtime owner, before its ordinary identity-bound removal. A live
/// or explicitly preserved runtime never reaches this function. Unknown markers fail closed.
pub(crate) fn recover_worker_storage(root: &Path) -> Result<(), String> {
    let marker = root.join(MARKER);
    let metadata = match std::fs::symlink_metadata(&marker) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
        Ok(metadata) => metadata,
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 8192 {
        return Err("Unsafe Worker storage recovery marker".into());
    }
    let lease: WorkerStorageLeaseV1 =
        serde_json::from_slice(&std::fs::read(&marker).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    lease.validate()?;
    let storage = Storage {
        runtime: lease.runtime.into(),
        home: root.join("home"),
        marker,
        name: lease.name,
        cleanup_attempted: std::cell::Cell::new(false),
    };
    storage.cleanup()
}

/// A deliberately closed archive reader: regular files and directories only, no links,
/// devices, arbitrary extensions, metadata restoration or extraction via a host tar executable.
fn extract(
    reader: &mut dyn Read,
    destination: &Path,
    allowed: &BTreeSet<String>,
    max_bytes: u64,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let invalid = || std::io::Error::other("Unsafe or over-bound Worker storage export");
    let mut seen = BTreeSet::new();
    let mut bytes = 0u64;
    let mut long_name: Option<Vec<u8>> = None;
    loop {
        let mut header = [0u8; 512];
        reader.read_exact(&mut header)?;
        if header.iter().all(|byte| *byte == 0) {
            let mut tail = Vec::new();
            reader.take(10_241).read_to_end(&mut tail)?;
            if long_name.is_some()
                || tail.len() > 10_240
                || tail.len() < 512
                || tail.iter().any(|byte| *byte != 0)
            {
                return Err(invalid());
            }
            break;
        }
        let checksum = octal(&header[148..156])?;
        let actual: u64 = header
            .iter()
            .enumerate()
            .map(|(i, byte)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(*byte)
                }
            })
            .sum();
        let gnu = &header[257..263] == b"ustar ";
        if checksum != actual || (!gnu && &header[257..263] != b"ustar\0") {
            return Err(invalid());
        }
        let field = |field: &[u8]| {
            let end = field
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(field.len());
            field[..end].to_vec()
        };
        let size = octal(&header[124..136])?;
        if header[156] == b'L' {
            if !gnu
                || long_name.is_some()
                || field(&header[..100]) != b"././@LongLink"
                || !(1..=4097).contains(&size)
            {
                return Err(invalid());
            }
            let mut name = vec![0; size as usize];
            reader.read_exact(&mut name)?;
            if name.pop() != Some(0) || name.contains(&0) {
                return Err(invalid());
            }
            let mut padding = [0u8; 512];
            reader.read_exact(&mut padding[..((512 - size % 512) % 512) as usize])?;
            long_name = Some(name);
            continue;
        }
        let mut name = if let Some(name) = long_name.take() {
            name
        } else {
            let name = field(&header[..100]);
            let mut prefix = if gnu {
                vec![]
            } else {
                field(&header[345..500])
            };
            if prefix.is_empty() {
                name
            } else {
                prefix.push(b'/');
                prefix.extend(name);
                prefix
            }
        };
        if name.last() == Some(&b'/') {
            name.pop();
        }
        let parts: Vec<_> = name.split(|byte| *byte == b'/').collect();
        if name.len() > 4096
            || name.contains(&0)
            || !parts
                .iter()
                .all(|part| !part.is_empty() && *part != b"." && *part != b"..")
            || !std::str::from_utf8(parts[0]).is_ok_and(|root| allowed.contains(root))
            || !seen.insert(name.clone())
            || seen.len() > MAX_ENTRIES
            || parts.len() > 64
        {
            return Err(invalid());
        }
        // Use the filesystem bytes directly, just as Manifest materialization does; later
        // scanning stores af_contracts::encode_path's one canonical spelling.
        use std::os::unix::ffi::OsStrExt;
        let path = destination.join(std::ffi::OsStr::from_bytes(&name));
        if path.parent().is_none_or(|parent| !parent.is_dir()) {
            return Err(invalid());
        }
        let size = octal(&header[124..136])?;
        match header[156] {
            b'5' if size == 0 => std::fs::create_dir(&path)?,
            b'0' | 0 if name.contains(&b'/') => {
                bytes = bytes
                    .checked_add(size)
                    .filter(|bytes| *bytes <= max_bytes)
                    .ok_or_else(invalid)?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                let copied = std::io::copy(&mut reader.take(size), &mut file)?;
                if copied != size {
                    return Err(invalid());
                }
                let mode = if octal(&header[100..108])? & 0o111 != 0 {
                    0o700
                } else {
                    0o600
                };
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
                let mut padding = [0u8; 512];
                reader.read_exact(&mut padding[..((512 - size % 512) % 512) as usize])?;
            }
            _ => return Err(invalid()),
        }
    }
    if !allowed.iter().all(|name| destination.join(name).is_dir()) {
        return Err(invalid());
    }
    Ok(())
}

fn octal(bytes: &[u8]) -> std::io::Result<u64> {
    let text = std::str::from_utf8(bytes)
        .map_err(std::io::Error::other)?
        .trim_matches(['\0', ' ']);
    u64::from_str_radix(text, 8).map_err(std::io::Error::other)
}

fn diagnostic(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if af_telemetry::redact::looks_like_credential(&text) {
        "[omitted: storage diagnostic contains credential-shaped material]".into()
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: u8, size: u64, content: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000700\0");
        header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[148..156].fill(b' ');
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        let sum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(content);
        if content.len() as u64 == size {
            bytes.resize(bytes.len().next_multiple_of(512), 0);
        }
        bytes
    }

    #[test]
    fn bounded_export_refuses_links_escapes_duplicates_sparse_sizes_and_aggregate_overflow() {
        let allowed = BTreeSet::from(["source".into()]);
        let root = entry("source/", b'5', 0, &[]);
        for bad in [
            entry("source/link", b'2', 0, &[]),
            entry("source/hard", b'1', 0, &[]),
            entry("source/device", b'3', 0, &[]),
            entry("source/pipe", b'6', 0, &[]),
            entry("source/../escape", b'0', 1, b"x"),
            entry("/source/absolute", b'0', 1, b"x"),
            entry("source/missing/parent", b'0', 1, b"x"),
            entry("source/sparse", b'0', 1 << 30, &[]),
            entry("source/truncated", b'0', 8, b"x"),
            [
                entry("source/a", b'0', 4, b"1234"),
                entry("source/b", b'0', 5, b"56789"),
            ]
            .concat(),
            [
                entry("source/a", b'0', 1, b"x"),
                entry("source/a", b'0', 1, b"y"),
            ]
            .concat(),
        ] {
            let stage = tempfile::tempdir().unwrap();
            let archive = [root.clone(), bad, vec![0; 1024]].concat();
            assert!(extract(&mut archive.as_slice(), stage.path(), &allowed, 8).is_err());
            assert!(!stage.path().join("escape").exists());
            assert!(
                !stage.path().join("source/sparse").exists(),
                "logical size refused before creating the file"
            );
        }
        let stage = tempfile::tempdir().unwrap();
        let archive = [root, entry("source/a", b'0', 8, b"12345678"), vec![0; 1024]].concat();
        extract(&mut archive.as_slice(), stage.path(), &allowed, 8).unwrap();
        assert_eq!(
            std::fs::read(stage.path().join("source/a")).unwrap(),
            b"12345678"
        );
    }

    #[test]
    fn bounded_gnu_long_names_preserve_manifest_paths_and_cannot_hide_traversal() {
        let allowed = BTreeSet::from(["source".into()]);
        let long = |name: &str| {
            let mut content = name.as_bytes().to_vec();
            content.push(0);
            let mut header = entry("././@LongLink", b'L', content.len() as u64, &content);
            header[257..263].copy_from_slice(b"ustar ");
            header[148..156].fill(b' ');
            let sum: u64 = header[..512].iter().map(|byte| u64::from(*byte)).sum();
            header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            header
        };
        let name = format!("source/{}", "a".repeat(200));
        let stage = tempfile::tempdir().unwrap();
        let archive = [
            entry("source/", b'5', 0, &[]),
            long(&name),
            entry("source/truncated", b'0', 1, b"x"),
            vec![0; 1024],
        ]
        .concat();
        extract(&mut archive.as_slice(), stage.path(), &allowed, 1).unwrap();
        assert_eq!(std::fs::read(stage.path().join(&name)).unwrap(), b"x");
        for invalid in [
            "source/../../escape".to_owned(),
            format!("source/{}", "x".repeat(4096)),
        ] {
            let stage = tempfile::tempdir().unwrap();
            let archive = [
                entry("source/", b'5', 0, &[]),
                long(&invalid),
                entry("source/innocent", b'0', 1, b"x"),
                vec![0; 1024],
            ]
            .concat();
            assert!(extract(&mut archive.as_slice(), stage.path(), &allowed, 1).is_err());
            assert!(!stage.path().join("escape").exists());
        }
    }

    #[test]
    fn unsupported_storage_backend_refuses_before_any_worker_dispatch_and_cleans_intent() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        super::super::tests::write_runtime_within(
            &runtime,
            "#!/bin/sh\ncase \"$1:$2\" in info:*) exit 0;; volume:create) echo 'local tmpfs driver unavailable' >&2; exit 42;; rm:*|volume:rm) exit 0;; *) echo 'unexpected Worker dispatch' >&2; exit 91;; esac\n",
            Duration::from_secs(120),
        );
        for name in ["home", "source", "package"] {
            std::fs::create_dir(directory.path().join(name)).unwrap();
        }
        let provider =
            ContainerProvider::with_runtime(&runtime).with_worker_layout(WorkerContainerLayout {
                package: directory.path().join("package"),
                roots: vec![],
                source_writable: true,
                base: None,
                processes: 32,
                disk_bytes: 4096,
                probe_shell: "/bin/sh".into(),
            });
        let error = provider
            .exec_worker_captured(
                &directory.path().join("source"),
                "/bin/sh",
                &[],
                &[],
                vec![],
                Instant::now() + Duration::from_secs(120),
                None,
            )
            .err()
            .unwrap();
        assert!(error.contains("local tmpfs driver unavailable"), "{error}");
        assert!(!directory.path().join(MARKER).exists());
        assert!(!directory.path().join("preserved").exists());
        let incompatible = provider
            .with_check_runtime()
            .exec_worker_captured(
                &directory.path().join("source"),
                "/bin/sh",
                &[],
                &[],
                vec![],
                Instant::now() + Duration::from_secs(120),
                None,
            )
            .err()
            .unwrap();
        assert!(incompatible.contains("independent check tmpfs"));
    }

    #[test]
    fn a_failed_recovery_keeps_its_intent_and_cannot_remove_an_unrelated_runtime() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(MARKER),
            br#"{"schema":"af.worker-storage/0","runtime":"/bin/false","name":"af-storage-owned"}"#,
        )
        .unwrap();
        assert!(recover_worker_storage(directory.path()).is_err());
        assert!(directory.path().join(MARKER).is_file());
        std::fs::remove_file(directory.path().join(MARKER)).unwrap();
        let outside = directory.path().join("outside");
        std::fs::write(&outside, b"private").unwrap();
        std::os::unix::fs::symlink(&outside, directory.path().join(MARKER)).unwrap();
        assert!(recover_worker_storage(directory.path()).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"private");
    }

    #[test]
    fn unconfirmed_cleanup_preserves_the_runtime_intent_and_source_without_retrying() {
        let runtime = crate::CheckRuntime::new().unwrap();
        let root = runtime.path().to_owned();
        let engine = root.join("engine");
        super::super::tests::write_runtime_within(
            &engine,
            "#!/bin/sh\ncase \"$1\" in rm) printf x >> \"$HOME/../cleanup-calls\"; exit 1;; ps) printf 'still-owned'; exit 0;; *) exit 91;; esac\n",
            Duration::from_secs(120),
        );
        std::fs::write(root.join("source"), b"original source").unwrap();
        let lease = WorkerStorageLeaseV1 {
            schema: SCHEMA.into(),
            runtime: engine.to_str().unwrap().into(),
            name: "af-storage-fixture".into(),
        };
        std::fs::write(root.join(MARKER), serde_json::to_vec(&lease).unwrap()).unwrap();
        assert!(recover_worker_storage(&root).is_err());
        assert_eq!(
            std::fs::read(root.join("cleanup-calls")).unwrap(),
            b"x",
            "cleanup has one bounded attempt"
        );
        drop(runtime);
        assert!(root.join(MARKER).is_file());
        assert!(root.join("preserved").is_file());
        assert_eq!(
            std::fs::read(root.join("source")).unwrap(),
            b"original source"
        );
        // This fixture has no real daemon resources and owns exactly this preserved directory.
        std::fs::remove_dir_all(root).unwrap();
    }
}
