//! The container provider: real isolation where a runtime exists, and a refusal where it does not.
//!
//! # Detection is a probe, not a lookup
//!
//! Finding `docker` on `PATH` proves nothing. On the machine this was written, both `docker` and
//! `podman` are installed and **neither daemon is reachable** — a provider that stopped at
//! `which` would have declared `Isolation::Container` and run every safe pipeline in no isolation
//! at all, which is the precise failure the isolation levels exist to prevent.
//!
//! So detection runs the runtime's own `info` and requires it to succeed. An unusable runtime is
//! [`Availability::Unusable`] with the reason attached, and a pipeline that required containment
//! is refused rather than quietly downgraded. That is the same rule as everywhere else here: a
//! capability that could not be verified is not a capability.
//!
//! # What the tests here do and do not prove
//!
//! The Gate invocation this builds — `--network=none`, a single bind of the sandbox, no inherited
//! host environment, no capabilities, no privilege gain, a process ceiling and an init that
//! reaps — is asserted exactly, using a recording stub in place of the runtime. That proves the
//! *plumbing*: the right flags, the right mount, nothing extra. `--env-file /dev/null` keeps the
//! host's environment out; it does not clear the `ENV` the image itself declares.
//! Opt-in Worker development uses subpaths of one size-limited private tmpfs volume, with
//! read-only package/Base, a read-only image, pinned entrypoint and no image acquisition.
//!
//! It does **not** prove containment. Only a real runtime can do that, and
//! `tests/container_probes.rs` does exactly that — the containment probes that need isolation
//! (`docs/security/containment-probes.md`), run against a live daemon locally and in CI, each
//! paired with a control proving the container genuinely runs work.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use af_process::{
    ExitPolicy, SupervisedCapture, SupervisedError, run_supervised, run_supervised_captured_bounded,
};

use crate::{Isolation, Mode, Sandbox, SandboxTemplate};

#[path = "container_storage.rs"]
mod storage;
pub(crate) use storage::recover_worker_storage;

/// Runtimes tried in order. Docker first only because it is the likeliest to be present.
const RUNTIMES: [&str; 3] = ["docker", "podman", "nerdctl"];

/// Capability detection runs in every full verification gate. A wedged daemon is unavailable,
/// not authority to keep the gate open forever.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// A timed-out runtime client is not the workload: the daemon-owned container must be stopped
/// before its writable bind can be sealed. Cleanup is bounded too, so a wedged daemon cannot
/// replace the check deadline with an unbounded second wait.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The most processes one container check may hold at once. A fork bomb ends at this ceiling
/// inside the container's own pids cgroup instead of filling the host's process table.
pub const DEFAULT_PIDS_LIMIT: u32 = 4096;

/// The default sandbox image, pinned by manifest digest — the same never-`latest` rule as
/// reviewer packages. The digest names a multi-arch manifest list (amd64 CI, arm64 laptops),
/// so one constant serves both. Debian `stable-slim` as of 2026-08-17; updating it is an
/// explicit edit here, never a tag quietly moving underneath a run.
pub const DEFAULT_IMAGE: &str = "docker.io/library/debian@sha256:1710bde34461551a19a47c787885ec9ad7058d9a5bead2affb8d088fa2f8502b";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// The runtime exists and answered its own status probe.
    Usable { runtime: PathBuf },
    /// A runtime binary exists but is not usable — most often a daemon that is not running.
    Unusable { runtime: PathBuf, reason: String },
    /// No runtime binary at all.
    Absent,
}

impl Availability {
    pub fn usable(&self) -> bool {
        matches!(self, Availability::Usable { .. })
    }

    pub fn reason(&self) -> String {
        match self {
            Availability::Usable { runtime } => format!("{} is usable", runtime.display()),
            Availability::Unusable { runtime, reason } => {
                format!("{} is installed but unusable: {reason}", runtime.display())
            }
            Availability::Absent => format!(
                "no container runtime found on PATH (tried {})",
                RUNTIMES.join(", ")
            ),
        }
    }
}

#[derive(Clone)]
pub struct ContainerProvider {
    availability: Availability,
    image: String,
    /// Mount the check runtime's `tmpfs` directories (see [`crate::CheckRuntime`]).
    check_runtime: bool,
    /// `--memory` and `--memory-swap`, when the Gate binding names a ceiling.
    memory: Option<String>,
    /// `--cpus`, when the Gate binding names a ceiling.
    cpus: Option<String>,
    /// The bytes kept of each output stream of a container command.
    output_cap: usize,
    worker: Option<WorkerContainerLayout>,
    image_control: bool,
    identity_probe: bool,
    storage_volume: Option<String>,
}

/// Kernel-owned private roots. They are never accepted from Worker TOML or argv.
#[derive(Clone)]
pub struct WorkerContainerLayout {
    pub package: PathBuf,
    pub roots: Vec<(PathBuf, String, bool)>,
    pub source_writable: bool,
    pub base: Option<PathBuf>,
    pub processes: u64,
    pub disk_bytes: u64,
    pub probe_shell: String,
}

pub struct WorkerContainerCapture {
    pub capture: SupervisedCapture,
    pub cleanup_confirmed: bool,
    /// False when a stopped workload's bounded filesystem export was unavailable. This is
    /// separate from daemon cleanup: the original host tree is then not final evidence.
    pub filesystem_exported: bool,
}

#[derive(Debug)]
pub struct ContainerExecution {
    pub output: std::process::Output,
    pub stderr_held: bool,
}

#[derive(Debug)]
pub struct ContainerExecutionError {
    detail: String,
    cleanup_confirmed: bool,
}

impl ContainerExecutionError {
    fn before_launch(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            cleanup_confirmed: true,
        }
    }

    fn after_launch(detail: impl Into<String>, cleanup_confirmed: bool) -> Self {
        Self {
            detail: detail.into(),
            cleanup_confirmed,
        }
    }

    /// False means the writable bind may still have a daemon-owned process attached. A caller
    /// must neither seal nor delete that sandbox.
    pub fn cleanup_confirmed(&self) -> bool {
        self.cleanup_confirmed
    }
}

impl std::fmt::Display for ContainerExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for ContainerExecutionError {}

impl ContainerProvider {
    /// Probe the host for a usable runtime.
    pub fn detect() -> ContainerProvider {
        ContainerProvider {
            availability: Self::probe(None),
            image: DEFAULT_IMAGE.to_string(),
            check_runtime: false,
            memory: None,
            cpus: None,
            output_cap: af_process::MAX_CAPTURED_STREAM_BYTES,
            worker: None,
            image_control: false,
            identity_probe: false,
            storage_volume: None,
        }
    }

    /// Every runtime probe consumes the caller's same absolute execution deadline.
    pub fn detect_before(deadline: Instant) -> ContainerProvider {
        ContainerProvider {
            availability: Self::probe(Some(deadline)),
            image: DEFAULT_IMAGE.to_string(),
            check_runtime: false,
            memory: None,
            cpus: None,
            output_cap: af_process::MAX_CAPTURED_STREAM_BYTES,
            worker: None,
            image_control: false,
            identity_probe: false,
            storage_volume: None,
        }
    }

    /// Point at a specific runtime binary, probing it the same way. Used by tests to drive both
    /// branches without depending on what happens to be installed.
    pub fn with_runtime(path: impl AsRef<Path>) -> ContainerProvider {
        let path = path.as_ref().to_path_buf();
        ContainerProvider {
            availability: Self::probe_one(&path),
            image: DEFAULT_IMAGE.to_string(),
            check_runtime: false,
            memory: None,
            cpus: None,
            output_cap: af_process::MAX_CAPTURED_STREAM_BYTES,
            worker: None,
            image_control: false,
            identity_probe: false,
            storage_volume: None,
        }
    }

    /// A Worker preparation probe consumes its Attempt's existing absolute deadline.
    pub fn with_runtime_before(
        path: impl AsRef<Path>,
        deadline: Instant,
        private_home: &Path,
    ) -> ContainerProvider {
        let path = path.as_ref().to_path_buf();
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(PROBE_TIMEOUT);
        let availability = if timeout.is_zero() {
            Availability::Absent
        } else {
            let mut command = std::process::Command::new(&path);
            command.args(["info", "--format", "{{json .}}"]);
            private_client(&mut command, private_home);
            let captured = run_supervised_captured_bounded(
                &mut command,
                None,
                timeout,
                ExitPolicy::KillDescendants,
                None,
                32 * 1024,
            );
            let capabilities = serde_json::from_slice::<serde_json::Value>(&captured.stdout).ok();
            let enforceable = capabilities.as_ref().is_some_and(|info| {
                ["MemoryLimit", "SwapLimit", "CpuCfsQuota", "PidsLimit"]
                    .into_iter()
                    .all(|field| info.get(field).and_then(serde_json::Value::as_bool) == Some(true))
            });
            if captured
                .status
                .as_ref()
                .is_ok_and(|status| status.success())
                && !captured.stderr_held
                && !captured.truncation.is_truncated()
                && enforceable
            {
                Availability::Usable {
                    runtime: path.clone(),
                }
            } else {
                Availability::Unusable {runtime:path.clone(),reason:"private local runtime did not prove bounded CPU, memory, swap and process enforcement".into()}
            }
        };
        ContainerProvider {
            availability,
            image: DEFAULT_IMAGE.to_string(),
            check_runtime: false,
            memory: None,
            cpus: None,
            output_cap: af_process::MAX_CAPTURED_STREAM_BYTES,
            worker: None,
            image_control: false,
            identity_probe: false,
            storage_volume: None,
        }
    }

    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    /// Cap the container's memory (swap included: `--memory-swap` equals `--memory`) and its
    /// CPUs, each only when the Gate binding names a value; the binding validated both.
    pub fn with_limits(mut self, memory: Option<String>, cpus: Option<String>) -> Self {
        self.memory = memory;
        self.cpus = cpus;
        self
    }

    /// Worker execution is opt-in: Gates retain their existing mount and runtime contract.
    pub fn with_worker_layout(mut self, layout: WorkerContainerLayout) -> Self {
        self.worker = Some(layout);
        self
    }

    fn without_worker_mounts(mut self) -> Self {
        self.image_control = true;
        self
    }

    /// Identity preflight sees an empty source and package: even an image symlink or
    /// interpreter startup setting cannot turn the probe into execution of project bytes.
    pub fn with_identity_probe_root(mut self, empty_root: &Path) -> Self {
        self.identity_probe = true;
        if let Some(worker) = &mut self.worker {
            worker.package = empty_root.to_owned();
            worker.roots.clear();
            worker.base = None;
            worker.source_writable = false;
        }
        self
    }

    /// Keep at most `cap` bytes of each output stream instead of the shared default.
    /// A command that writes more fails rather than returning a partial successful output.
    pub fn with_output_cap(mut self, cap: usize) -> Self {
        self.output_cap = cap;
        self
    }

    /// Give every command a check's runtime directories: one empty, writable `tmpfs` per
    /// variable of [`crate::CheckRuntime::container_environment`], gone with the container. The
    /// sandbox stays the only bind.
    pub fn with_check_runtime(mut self) -> Self {
        self.check_runtime = true;
        self
    }

    fn probe(deadline: Option<Instant>) -> Availability {
        Self::probe_paths(
            RUNTIMES.into_iter().filter_map(|name| which(name).ok()),
            deadline,
        )
    }

    fn probe_paths(
        paths: impl IntoIterator<Item = PathBuf>,
        deadline: Option<Instant>,
    ) -> Availability {
        let mut first_unusable = None;
        for path in paths {
            let timeout = deadline.map_or(PROBE_TIMEOUT, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(PROBE_TIMEOUT)
            });
            if timeout.is_zero() {
                break;
            }
            match Self::probe_one_with_timeout(&path, timeout) {
                usable @ Availability::Usable { .. } => return usable,
                unusable if first_unusable.is_none() => first_unusable = Some(unusable),
                _ => {}
            }
        }
        first_unusable.unwrap_or(Availability::Absent)
    }

    /// The probe itself: ask the runtime to describe itself, and require success.
    fn probe_one(path: &Path) -> Availability {
        Self::probe_one_with_timeout(path, PROBE_TIMEOUT)
    }

    fn probe_one_with_timeout(path: &Path, timeout: Duration) -> Availability {
        if !path.exists() {
            return Availability::Absent;
        }
        let output = run_probe(path, timeout);
        match output {
            Ok(output) if output.status.success() => Availability::Usable {
                runtime: path.to_path_buf(),
            },
            Ok(output) => Availability::Unusable {
                runtime: path.to_path_buf(),
                reason: String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("`info` failed")
                    .trim()
                    .to_string(),
            },
            Err(error) => Availability::Unusable {
                runtime: path.to_path_buf(),
                reason: error.to_string(),
            },
        }
    }

    pub fn availability(&self) -> &Availability {
        &self.availability
    }

    /// The isolation this provider *actually* offers right now.
    ///
    /// `None` when the runtime is missing or unusable — so a safe pipeline is refused by the
    /// ordinary [`crate::admit`] check rather than by a special case someone has to remember.
    pub fn isolation(&self) -> Isolation {
        if self.availability.usable() {
            Isolation::Container
        } else {
            Isolation::None
        }
    }

    /// Clone one node sandbox and attach only the isolation this probed provider can actually
    /// supply. An unavailable runtime therefore creates an ordinary `Isolation::None` sandbox;
    /// policy admission still refuses it before any project command executes.
    pub fn sandbox_from_template(
        &self,
        template: &SandboxTemplate,
        mode: Mode,
    ) -> Result<Sandbox, std::io::Error> {
        Sandbox::from_template_with_isolation(template, mode, self.isolation())
    }

    /// The exact argv for running a command inside the sandbox.
    ///
    /// Built even when the runtime is unusable, because it is the part worth asserting: one bind,
    /// no network, only declared environment, no capabilities and no privilege gain, a process
    /// ceiling, an init that reaps, a reaping identity, and the command in a value position after
    /// the image.
    pub fn invocation(
        &self,
        sandbox_root: &Path,
        program: &str,
        args: &[String],
        environment: &[(String, String)],
        execution_name: &str,
    ) -> Vec<String> {
        let mut argv = vec![
            "run".to_string(),
            "--rm".to_string(),
            // The runtime client is not the daemon-owned workload. A stable name lets the
            // provider stop that workload if supervision kills the client on its deadline.
            "--name".to_string(),
            execution_name.to_string(),
            // No undeclared network. This is the containment probe no weaker provider can close.
            "--network=none".to_string(),
            // No ambient host environment crosses in. Only the kernel-owned allowlist below is
            // reintroduced explicitly.
            "--env-file".to_string(),
            "/dev/null".to_string(),
            // A fork bomb ends at the container's own ceiling, not the host's process table.
            "--pids-limit".to_string(),
            self.worker
                .as_ref()
                .map_or(u64::from(DEFAULT_PIDS_LIMIT), |worker| worker.processes)
                .to_string(),
            // A check needs no capability, and nothing it runs may gain privilege (setuid).
            "--cap-drop=ALL".to_string(),
            "--security-opt=no-new-privileges".to_string(),
            // An init as PID 1 reaps orphans and forwards signals, so a stop ends the check.
            "--init".to_string(),
        ];
        if let Some(memory) = &self.memory {
            argv.extend([
                "--memory".to_string(),
                memory.clone(),
                "--memory-swap".to_string(),
                memory.clone(),
            ]);
        }
        if let Some(cpus) = &self.cpus {
            argv.extend(["--cpus".to_string(), cpus.clone()]);
        }
        if let Some(worker) = &self.worker {
            // Never acquire an image as an accidental dependency miss. Rootfs and package
            // bytes are immutable; only the exact private roots below are writable.
            argv.extend([
                // Forward the host-supervised input pipe to the harness. Without this,
                // the runtime client accepts bytes but the container sees immediate EOF.
                // No TTY: stdout/stderr must remain separate, exact receipt bytes.
                "--interactive".into(),
                "--pull=never".into(),
                "--read-only".into(),
                "--no-healthcheck".into(),
                "--entrypoint".into(),
                program.into(),
                // No daemon log file can grow outside the admitted storage ceiling.
                "--log-driver=none".into(),
                "--ipc=none".into(),
            ]);
            if !self.image_control {
                argv.extend([
                    "--volume".into(),
                    format!("{}:/af/package:ro", worker.package.display()),
                ]);
                for (host, name, writable) in &worker.roots {
                    if let Some(volume) = &self.storage_volume {
                        storage::mount(&mut argv, volume, name, &format!("/af/{name}"), !writable);
                    } else {
                        argv.extend([
                            "--volume".into(),
                            format!("{}:/af/{name}:{}", host.display(), "ro"),
                        ]);
                    }
                }
                if let Some(base) = &worker.base {
                    if let Some(volume) = &self.storage_volume {
                        storage::mount(&mut argv, volume, "base", "/base", true);
                    } else {
                        argv.extend(["--volume".into(), format!("{}:/base:ro", base.display())]);
                    }
                }
                if let Some(volume) = &self.storage_volume {
                    for (name, target) in [("tmp", "/tmp"), ("run", "/run"), ("shm", "/dev/shm")] {
                        storage::mount(&mut argv, volume, name, target, false);
                    }
                    // POSIX queues are not part of the filesystem quota. Hide their mount.
                    storage::mount(&mut argv, volume, "mqueue", "/dev/mqueue", true);
                }
            }
        }
        argv.extend([
            "--user".to_string(),
            if self.worker.is_some() {
                // Even when af itself runs as root, /dev's root-owned tmpfs is unwritable.
                worker_user()
            } else {
                format!(
                    "{}:{}",
                    nix::unistd::getuid().as_raw(),
                    nix::unistd::getgid().as_raw()
                )
            },
        ]);
        for (key, value) in environment {
            argv.push("-e".to_string());
            argv.push(format!("{key}={value}"));
        }
        if self.check_runtime && self.worker.is_none() {
            for mount in crate::CheckRuntime::container_mounts() {
                // Executable, because a check builds and runs test binaries in `TMPDIR`; world
                // writable with the sticky bit, because the mount is the daemon's and the
                // command runs as the invoking user.
                argv.push("--tmpfs".to_string());
                argv.push(format!("{mount}:rw,exec,mode=1777"));
            }
        }
        argv.extend([
            "--workdir".to_string(),
            if self.image_control {
                "/".to_string()
            } else {
                "/work".to_string()
            },
        ]);
        if !self.image_control {
            if let Some(volume) = &self.storage_volume {
                storage::mount(
                    &mut argv,
                    volume,
                    "source",
                    "/work",
                    !self.worker.as_ref().unwrap().source_writable,
                );
            } else {
                argv.extend([
                    "--volume".to_string(),
                    format!(
                        "{}:/work:{}",
                        sandbox_root.display(),
                        if self.worker.is_none() { "rw" } else { "ro" }
                    ),
                ]);
            }
        }
        argv.push(self.image.clone());
        if self.worker.is_none() {
            argv.push(program.to_string());
        }
        argv.extend(args.iter().cloned());
        argv
    }

    /// Image-declared volumes can shadow a bound source subtree with unrelated bytes.
    /// Refuse them before a Worker: its source Manifest must describe what /work actually sees.
    pub fn admit_worker_image(
        &self,
        private_home: &Path,
        probe_shell: &str,
        programs: &[String],
        deadline: Instant,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        let Availability::Usable { runtime } = &self.availability else {
            return Err("Worker development requires a usable local runtime".into());
        };
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or("Worker development deadline expired before image admission")?;
        let mut command = std::process::Command::new(runtime);
        command
            .args([
                "image",
                "inspect",
                "--format",
                "{{json .Config.Volumes}}",
                &self.image,
            ])
            .current_dir(private_home);
        private_client(&mut command, private_home);
        let captured = run_supervised_captured_bounded(
            &mut command,
            None,
            remaining,
            ExitPolicy::KillDescendants,
            cancellation,
            16 * 1024,
        );
        if !captured
            .status
            .as_ref()
            .is_ok_and(|status| status.success())
            || captured.stderr_held
            || captured.truncation.is_truncated()
        {
            return Err("Worker development image must already be available locally and support bounded metadata admission".into());
        }
        let no_volumes = match serde_json::from_slice::<serde_json::Value>(&captured.stdout) {
            Ok(serde_json::Value::Null) => true,
            Ok(serde_json::Value::Object(volumes)) => volumes.is_empty(),
            _ => false,
        };
        if !no_volumes {
            return Err("Worker development refuses image-declared volumes that could shadow exact source or private roots".into());
        }
        // The image is immutable and this invocation has no host mounts. Inspect every fixed
        // mount component before mounting source: an image symlink must not alias two roots.
        let script = "set -eu; for p in /work /af /af/package /af/state /af/state/home /af/state/config /af/scratch /af/build /af/cache /af/knowledge /af/dependencies /base /tmp /run /dev /dev/shm /dev/mqueue /disk /input /input/source /input/base /input/state /input/scratch /input/build /input/cache /input/knowledge; do if test -L \"$p\"; then exit 72; fi; if test -e \"$p\" && ! test -d \"$p\"; then exit 73; fi; done; for p do resolved=$(/usr/bin/readlink -f \"$p\") || exit 74; case \"$resolved\" in /work|/work/*|/base|/base/*|/af/source|/af/source/*|/af/state|/af/state/*|/af/scratch|/af/scratch/*|/af/build|/af/build/*|/af/cache|/af/cache/*|/af/knowledge|/af/knowledge/*|/af/package|/af/package/*|/input|/input/*|/disk|/disk/*|/tmp|/tmp/*|/run|/run/*|/proc|/proc/*|/sys|/sys/*|/dev|/dev/*) exit 75;; esac; test -x \"$resolved\" || exit 76; done";
        let mut arguments = vec![
            "-c".into(),
            script.into(),
            "af-image-admission".into(),
            probe_shell.into(),
        ];
        arguments.extend(
            [
                "/usr/bin/readlink",
                "/usr/bin/stat",
                "/bin/tar",
                "/bin/cp",
                "/bin/mkdir",
                "/bin/chmod",
                "/bin/chown",
                "/bin/sleep",
            ]
            .map(str::to_owned),
        );
        arguments.extend_from_slice(programs);
        let probe = self.clone().without_worker_mounts().exec_worker_captured(
            Path::new("/"),
            probe_shell,
            &arguments,
            &[],
            vec![],
            deadline,
            cancellation,
        )?;
        if !probe.cleanup_confirmed
            || !probe
                .capture
                .status
                .as_ref()
                .is_ok_and(|status| status.success())
            || probe.capture.stderr_held
            || probe.capture.truncation.is_truncated()
        {
            return Err("Worker development requires non-symlink image mount roots, immutable resolved tool paths and supported storage utilities".into());
        }
        Ok(())
    }

    /// The same supervisor and daemon cleanup as a Gate, preserving bounded bytes and
    /// truncation metadata for a Worker receipt. Input goes on stdin, never in argv.
    #[allow(clippy::too_many_arguments)]
    pub fn exec_worker_captured(
        &self,
        sandbox_root: &Path,
        program: &str,
        args: &[String],
        environment: &[(String, String)],
        input: Vec<u8>,
        deadline: Instant,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<WorkerContainerCapture, String> {
        if self.worker.is_none() {
            return Err("Worker container requires its private layout".into());
        }
        if self.check_runtime {
            return Err(
                "Worker storage cannot be combined with independent check tmpfs mounts".into(),
            );
        }
        let Availability::Usable { runtime } = &self.availability else {
            return Err(format!(
                "Worker development requires a usable container runtime: {}",
                self.availability.reason()
            ));
        };
        let identity = tempfile::Builder::new()
            .prefix("af-worker-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let temporary_name = identity
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Non-UTF-8 Worker execution identity")?;
        // The runtime client receives no host credentials or configuration. This backend
        // supports only a local default daemon; remote/runtime-context configuration is absent.
        let client_home = self
            .worker
            .as_ref()
            .unwrap()
            .package
            .parent()
            .ok_or("Worker package has no runtime parent")?
            .join("home");
        std::fs::create_dir_all(&client_home).map_err(|error| error.to_string())?;
        let storage = if !self.image_control && !self.identity_probe {
            Some(storage::Storage::create(
                self,
                sandbox_root,
                &client_home,
                deadline,
                cancellation,
            )?)
        } else {
            None
        };
        let name = storage
            .as_ref()
            .map(|storage| format!("{}-worker", storage.name))
            .unwrap_or_else(|| temporary_name.to_owned());
        let mut prepared = self.clone();
        prepared.storage_volume = storage.as_ref().map(|storage| storage.name.clone());
        let mut command = std::process::Command::new(runtime);
        command.args(prepared.invocation(sandbox_root, program, args, environment, &name));
        private_client(&mut command, &client_home);
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or("Worker development deadline expired before launch")?;
        let mut capture = run_supervised_captured_bounded(
            &mut command,
            Some(input),
            remaining,
            ExitPolicy::KillDescendants,
            cancellation,
            self.output_cap,
        );
        let mut cleanup_confirmed = if capture.status.as_ref().is_ok_and(|status| status.success())
            && !capture.stderr_held
        {
            true
        } else {
            remove_container_with_home(runtime, &name, Some(&client_home)).is_ok()
        };
        let mut filesystem_exported = cleanup_confirmed;
        if let Some(storage) = storage {
            if cleanup_confirmed
                && let Err(error) = storage.export(self, sandbox_root, deadline, cancellation)
            {
                capture.status = Err(SupervisedError::Wait(std::io::Error::other(error)));
                filesystem_exported = false;
            }
            cleanup_confirmed = storage.cleanup().is_ok();
        }
        Ok(WorkerContainerCapture {
            capture,
            cleanup_confirmed,
            filesystem_exported,
        })
    }

    /// Run a command in the sandbox under the caller's policy deadline, stopping early when
    /// `cancellation` is raised. Refuses when the runtime is not usable — never falls back to
    /// running it on the host, which would be containment silently becoming none.
    ///
    /// Preserves the process-supervision evidence CheckResult needs. In particular, a
    /// descendant that keeps stderr open must remain `not_run`, not turn into a passing check
    /// merely because the container runtime's leader exited.
    #[allow(clippy::too_many_arguments)]
    pub fn exec_evidenced_controlled(
        &self,
        sandbox_root: &Path,
        program: &str,
        args: &[String],
        environment: &[(String, String)],
        timeout: Duration,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<ContainerExecution, ContainerExecutionError> {
        if self.worker.is_some() {
            return Err(ContainerExecutionError::before_launch(
                "Worker layouts require bounded Worker dispatch",
            ));
        }
        if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(ContainerExecutionError::before_launch(
                "container execution cancelled before launch",
            ));
        }

        let Availability::Usable { runtime } = &self.availability else {
            return Err(ContainerExecutionError::before_launch(format!(
                "refusing to run outside a container: {}",
                self.availability.reason()
            )));
        };
        let identity = tempfile::Builder::new()
            .prefix("af-gate-")
            .tempdir()
            .map_err(|error| {
                ContainerExecutionError::before_launch(format!(
                    "allocating container execution identity: {error}"
                ))
            })?;
        let execution_name = identity
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                ContainerExecutionError::before_launch(
                    "container execution identity is not portable UTF-8",
                )
            })?;
        self.exec_evidenced_named_controlled(
            runtime,
            sandbox_root,
            program,
            args,
            environment,
            timeout,
            execution_name,
            cancellation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    fn exec_evidenced_named(
        &self,
        runtime: &Path,
        sandbox_root: &Path,
        program: &str,
        args: &[String],
        environment: &[(String, String)],
        timeout: Duration,
        execution_name: &str,
    ) -> Result<ContainerExecution, ContainerExecutionError> {
        self.exec_evidenced_named_controlled(
            runtime,
            sandbox_root,
            program,
            args,
            environment,
            timeout,
            execution_name,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_evidenced_named_controlled(
        &self,
        runtime: &Path,
        sandbox_root: &Path,
        program: &str,
        args: &[String],
        environment: &[(String, String)],
        timeout: Duration,
        execution_name: &str,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<ContainerExecution, ContainerExecutionError> {
        let mut command = std::process::Command::new(runtime);
        command.args(self.invocation(sandbox_root, program, args, environment, execution_name));
        let span = process_span(&command, "container command");
        let captured = span.in_scope(|| {
            run_supervised_captured_bounded(
                &mut command,
                None,
                timeout,
                ExitPolicy::PreserveProcessGroup,
                cancellation,
                self.output_cap,
            )
        });
        let executed = container_execution(&span, captured, timeout);
        // Cancellation stops the foreground runtime; the existing bounded rm confirms the
        // daemon no longer owns this writable bind. Never cancel the cleanup itself.
        match executed {
            Ok(execution) => Ok(execution),
            Err(error) => match remove_container(runtime, execution_name) {
                Ok(()) => Err(ContainerExecutionError::after_launch(
                    format!("{error}; container execution `{execution_name}` was forcibly removed"),
                    true,
                )),
                Err(cleanup) => Err(ContainerExecutionError::after_launch(
                    format!(
                        "{error}; cleanup of container execution `{execution_name}` was not confirmed: {cleanup}"
                    ),
                    false,
                )),
            },
        }
    }
}

/// A container command's capture as execution evidence. Output past the cap is an execution
/// failure naming the cap, so the check never passes on a prefix of its output. The command's
/// span ends after that judgement: a capture rejected for its truncation ends it `failed` with
/// the cap's reason.
fn container_execution(
    span: &tracing::Span,
    captured: SupervisedCapture,
    timeout: Duration,
) -> Result<ContainerExecution, std::io::Error> {
    let capped = captured
        .status
        .is_ok()
        .then(|| captured.truncation.reason())
        .flatten();
    record_capture(span, &captured, capped.as_deref());
    if let Some(reason) = capped {
        return Err(std::io::Error::other(format!("container command {reason}")));
    }
    captured
        .into_result()
        .map(|output| ContainerExecution {
            output: std::process::Output {
                status: output.status,
                stdout: output.stdout,
                stderr: output.stderr,
            },
            stderr_held: output.stderr_held,
        })
        .map_err(|error| match error {
            SupervisedError::TimedOut { .. } => std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "container command did not finish within {}s",
                    timeout.as_secs_f64()
                ),
            ),
            SupervisedError::Cancelled => std::io::Error::other(error),
            error => std::io::Error::other(format!("container command: {error}")),
        })
}

fn worker_user() -> String {
    if nix::unistd::getuid().is_root() {
        "65534:65534".into()
    } else {
        format!("{}:{}", nix::unistd::getuid(), nix::unistd::getgid())
    }
}

fn private_client(command: &mut std::process::Command, home: &Path) {
    command
        .env_clear()
        .env("PATH", af_process::sanitized_path())
        .env("HOME", home)
        .env("DOCKER_CONFIG", home)
        .env("XDG_CONFIG_HOME", home);
}

fn remove_container(runtime: &Path, execution_name: &str) -> Result<(), String> {
    remove_container_with_home(runtime, execution_name, None)
}

fn remove_container_with_home(
    runtime: &Path,
    execution_name: &str,
    private_home: Option<&Path>,
) -> Result<(), String> {
    let started = Instant::now();
    let mut command = std::process::Command::new(runtime);
    command.args(["rm", "-f", execution_name]);
    if let Some(home) = private_home {
        private_client(&mut command, home);
    }
    let execution = run_bounded_evidenced(command, CLEANUP_TIMEOUT, "container cleanup")
        .map_err(|error| error.to_string())?;
    if execution.output.status.success() {
        return Ok(());
    }
    // A failed Worker may already have been removed by --rm. Confirm absence through the
    // same private daemon context, within the original cleanup budget; never infer it from
    // localized stderr or run cleanup against the operator's ambient remote context.
    if let Some(home) = private_home {
        let remaining = CLEANUP_TIMEOUT.saturating_sub(started.elapsed());
        if !remaining.is_zero() {
            let mut query = std::process::Command::new(runtime);
            query.args([
                "ps",
                "--all",
                "--filter",
                &format!("name=^/{execution_name}$"),
                "--format",
                "{{.Names}}",
            ]);
            private_client(&mut query, home);
            if let Ok(absence) =
                run_bounded_evidenced(query, remaining, "Worker container absence check")
                && absence.output.status.success()
                && !absence.stderr_held
                && absence.output.stdout.iter().all(u8::is_ascii_whitespace)
            {
                return Ok(());
            }
        }
    }
    Err(format!(
        "runtime exited {:?}: {}",
        execution.output.status.code(),
        String::from_utf8_lossy(&execution.output.stderr).trim()
    ))
}

fn run_probe(path: &Path, timeout: Duration) -> Result<std::process::Output, std::io::Error> {
    let mut command = std::process::Command::new(path);
    command.arg("info");
    run_bounded(command, timeout, "runtime info probe")
}

fn run_bounded(
    command: std::process::Command,
    timeout: Duration,
    operation: &str,
) -> Result<std::process::Output, std::io::Error> {
    run_bounded_evidenced(command, timeout, operation).map(|execution| execution.output)
}

fn run_bounded_evidenced(
    mut command: std::process::Command,
    timeout: Duration,
    operation: &str,
) -> Result<ContainerExecution, std::io::Error> {
    let span = process_span(&command, operation);
    let output = span.in_scope(|| run_supervised(&mut command, None, timeout));
    record_output(&span, &output);
    output
        .map(|output| ContainerExecution {
            output: std::process::Output {
                status: output.status,
                stdout: output.stdout,
                stderr: output.stderr,
            },
            stderr_held: output.stderr_held,
        })
        .map_err(|error| match error {
            SupervisedError::TimedOut { .. } => std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "{operation} did not finish within {}s",
                    timeout.as_secs_f64()
                ),
            ),
            error => std::io::Error::other(format!("{operation}: {error}")),
        })
}

/// The `af.process` span of one container-runtime process (telemetry design §7.2): the runtime
/// program only, never its arguments, and what the process was for.
fn process_span(command: &std::process::Command, operation: &str) -> tracing::Span {
    if !af_telemetry::process::in_run() {
        return tracing::Span::none();
    }
    let program = Path::new(command.get_program())
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    af_telemetry::process_span!(
        "af.process",
        process.command = program.as_deref(),
        af.process.kind = operation,
    )
}

fn resource_use(rusage: af_process::Rusage) -> af_telemetry::process::ResourceUse {
    af_telemetry::process::ResourceUse {
        user_ms: rusage.user_ms,
        system_ms: rusage.system_ms,
        max_rss_bytes: rusage.max_rss_bytes,
    }
}

/// End a container process's span from its capture: `ok` when it exited 0 and the execution
/// kept it; `capped` is the cap reason when the execution rejected it for its truncation.
fn record_capture(
    span: &tracing::Span,
    captured: &af_process::SupervisedCapture,
    capped: Option<&str>,
) {
    if span.is_disabled() {
        return;
    }
    let mut facts = af_telemetry::process::ProcessFacts {
        pid: captured.pid,
        rusage: captured.rusage.map(resource_use),
        stdout_bytes: Some(captured.stdout.len() as u64),
        stderr_bytes: Some(captured.stderr.len() as u64),
        stderr_held: captured.stderr_held,
        ..Default::default()
    };
    if let Ok(status) = &captured.status {
        facts = facts.with_status(status);
    }
    finish_process(
        span,
        &facts,
        captured.status.as_ref().map(|status| *status),
        capped,
    );
}

/// End a container process's span from its bounded run.
fn record_output(
    span: &tracing::Span,
    output: &Result<af_process::SupervisedOutput, SupervisedError>,
) {
    if span.is_disabled() {
        return;
    }
    let facts = match output {
        Ok(output) => af_telemetry::process::ProcessFacts {
            pid: output.pid,
            rusage: output.rusage.map(resource_use),
            stdout_bytes: Some(output.stdout.len() as u64),
            stderr_bytes: Some(output.stderr.len() as u64),
            stderr_held: output.stderr_held,
            ..Default::default()
        }
        .with_status(&output.status),
        Err(SupervisedError::TimedOut {
            stdout,
            stderr,
            pid,
            rusage,
        }) => af_telemetry::process::ProcessFacts {
            pid: *pid,
            rusage: rusage.map(resource_use),
            stdout_bytes: Some(stdout.len() as u64),
            stderr_bytes: Some(stderr.len() as u64),
            ..Default::default()
        },
        Err(_) => af_telemetry::process::ProcessFacts::default(),
    };
    finish_process(
        span,
        &facts,
        output.as_ref().map(|output| output.status),
        None,
    );
}

fn finish_process(
    span: &tracing::Span,
    facts: &af_telemetry::process::ProcessFacts,
    status: Result<std::process::ExitStatus, &SupervisedError>,
    capped: Option<&str>,
) {
    let failure = match (status, capped) {
        (Ok(_), Some(reason)) => Some(("failed", reason.to_string())),
        (Ok(status), None) if status.success() => None,
        (Ok(_), None) => Some(("failed", "the runtime exited unsuccessfully".to_string())),
        (Err(SupervisedError::TimedOut { .. }), _) => {
            Some(("timed_out", "the runtime exceeded its deadline".to_string()))
        }
        (Err(SupervisedError::Cancelled), _) => Some(("refused", "cancelled".to_string())),
        (Err(SupervisedError::Spawn(_)), _) => Some((
            "unavailable",
            "the runtime could not be started".to_string(),
        )),
        (Err(error), _) => Some(("failed", error.to_string())),
    };
    af_telemetry::process::finish(
        span,
        facts,
        failure
            .as_ref()
            .map(|(class, reason)| (*class, reason.as_str())),
    );
}

fn which(name: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The fixture-writing wall for a test whose subject is not a deadline (ADR-0114). Under a
    /// loaded gate a shell took seconds to start, so a 5 s wall raced scheduling; two minutes
    /// only bounds a hung writer, and a passing test never waits for it. Tests whose subject is
    /// a probe or execution deadline keep the 5 s writer wall.
    const LOAD_SAFE_WALL: Duration = Duration::from_secs(120);

    fn write_runtime(path: &Path, script: &str) {
        write_runtime_within(path, script, Duration::from_secs(5));
    }

    pub(super) fn write_runtime_within(path: &Path, script: &str, wall: Duration) {
        // Linux CI observed ETXTBSY while probing a freshly written fixture. A concurrent
        // child's inherited writable descriptor is a possible cause. Keep that descriptor
        // out of this multithreaded parent; reaping this single-threaded writer establishes
        // that its writable descriptor is closed before probing the executable.
        let mut writer = std::process::Command::new("/bin/sh");
        writer
            .args(["-c", "printf '%s' \"$2\" > \"$1\"", "runtime-fixture"])
            .arg(path)
            .arg(script);
        let output = run_supervised(&mut writer, None, wall).unwrap();
        assert!(
            output.status.success(),
            "writing runtime fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// The real host, whatever it is. Both outcomes are correct; what matters is that the
    /// provider never claims containment it has not verified.
    #[test]
    fn detection_never_claims_more_than_it_probed() {
        let provider = ContainerProvider::detect();
        match provider.availability() {
            Availability::Usable { .. } => {
                assert_eq!(provider.isolation(), Isolation::Container)
            }
            unusable => {
                assert_eq!(
                    provider.isolation(),
                    Isolation::None,
                    "an unusable runtime must not claim containment: {}",
                    unusable.reason()
                );
            }
        }
    }

    #[test]
    fn an_installed_but_broken_runtime_is_unusable_not_usable() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("broken-runtime");
        write_runtime_within(
            &fake,
            "#!/bin/sh\necho 'Cannot connect to the daemon' >&2\nexit 1\n",
            LOAD_SAFE_WALL,
        );

        let provider = ContainerProvider::with_runtime(&fake);
        assert!(matches!(
            provider.availability(),
            Availability::Unusable { .. }
        ));
        assert_eq!(provider.isolation(), Isolation::None);
        assert!(provider.availability().reason().contains("daemon"));

        // And it refuses to run rather than falling back to the host.
        let err = provider
            .exec_evidenced_controlled(
                dir.path(),
                "/bin/sh",
                &["-c".into(), "echo pwned".into()],
                &[],
                Duration::from_secs(1),
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("refusing to run outside a container"),
            "{err}"
        );
    }

    #[test]
    fn runtime_detection_stops_at_the_callers_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..3)
            .map(|n| {
                let path = directory.path().join(format!("runtime{n}"));
                write_runtime(&path, "#!/bin/sh\nprintf x > \"$0.marker\"\nsleep 60\n");
                path
            })
            .collect();
        let start = Instant::now();
        let unavailable =
            ContainerProvider::probe_paths(paths.clone(), Some(start + Duration::from_millis(500)));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!unavailable.usable());
        // A busy host may time out before the first script reaches its body. The observed
        // attempted runtime and deadline failure are authoritative; a script marker is not.
        assert!(
            matches!(&unavailable, Availability::Unusable { runtime, reason }
            if runtime == &paths[0] && reason.contains("did not finish")),
            "{unavailable:?}"
        );
        assert!(!directory.path().join("runtime1.marker").exists());
        assert!(!directory.path().join("runtime2.marker").exists());
        let expired = ContainerProvider::probe_paths([paths[1].clone()], Some(start));
        assert!(!expired.usable());
        assert!(!directory.path().join("runtime1.marker").exists());
    }

    #[test]
    fn a_wedged_runtime_is_bounded_and_unusable() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("wedged-runtime");
        write_runtime(&fake, "#!/bin/sh\nsleep 60\n");

        let started = Instant::now();
        let availability =
            ContainerProvider::probe_one_with_timeout(&fake, Duration::from_millis(100));

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(availability, Availability::Unusable { .. }));
        assert!(availability.reason().contains("did not finish"));
    }

    #[test]
    fn a_wedged_container_execution_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("runtime");
        write_runtime(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nif [ \"$1\" = rm ]; then printf '%s\\n' \"$@\" > \"$0.cleanup\"; exit 0; fi\nsleep 60\n",
        );
        let provider = ContainerProvider::with_runtime(&fake);

        let started = Instant::now();
        let error = provider
            .exec_evidenced_controlled(
                dir.path(),
                "/bin/true",
                &[],
                &[],
                Duration::from_millis(100),
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            error.contains("container command did not finish"),
            "{error}"
        );
        assert!(error.contains("was forcibly removed"), "{error}");
        let cleanup = std::fs::read_to_string(format!("{}.cleanup", fake.display())).unwrap();
        assert!(cleanup.starts_with("rm\n-f\naf-gate-"), "{cleanup:?}");
    }

    #[test]
    fn a_failed_reap_is_distinct_from_a_safely_stopped_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("runtime");
        write_runtime(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nif [ \"$1\" = rm ]; then echo 'daemon lost' >&2; exit 1; fi\nsleep 60\n",
        );
        let provider = ContainerProvider::with_runtime(&fake);

        let error = provider
            .exec_evidenced_controlled(
                dir.path(),
                "/bin/true",
                &[],
                &[],
                Duration::from_millis(100),
                None,
            )
            .unwrap_err();
        assert!(!error.cleanup_confirmed(), "{error}");
        assert!(
            error
                .to_string()
                .contains("container command did not finish"),
            "{error}"
        );
        assert!(error.to_string().contains("was not confirmed"), "{error}");
    }

    #[test]
    fn cancellation_still_confirms_container_removal_without_cancelling_cleanup() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for cleanup_ok in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let fake = dir.path().join("runtime");
            write_runtime_within(
                &fake,
                &format!(
                    "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nif [ \"$1\" = rm ]; then printf '%s\\n' \"$@\" > \"$0.cleanup\"; exit {}; fi\nprintf ready > \"$0.ready\"\nsleep 30\n",
                    if cleanup_ok { 0 } else { 1 }
                ),
                LOAD_SAFE_WALL,
            );
            let provider = ContainerProvider::with_runtime(&fake);
            let flag = AtomicBool::new(false);
            let started = Instant::now();
            let error = std::thread::scope(|scope| {
                let cancel = scope.spawn(|| {
                    while !fake.with_extension("ready").is_file() {
                        if started.elapsed() > Duration::from_secs(3) {
                            flag.store(true, Ordering::Release);
                            panic!("container never began");
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    flag.store(true, Ordering::Release);
                });
                let result = provider.exec_evidenced_controlled(
                    dir.path(),
                    "/bin/true",
                    &[],
                    &[],
                    Duration::from_secs(20),
                    Some(&flag),
                );
                cancel.join().unwrap();
                result.unwrap_err()
            });
            assert!(started.elapsed() < Duration::from_secs(3));
            assert_eq!(error.cleanup_confirmed(), cleanup_ok);
            assert!(error.to_string().contains("cancel"), "{error}");
            let cleanup = std::fs::read_to_string(fake.with_extension("cleanup")).unwrap();
            assert!(cleanup.starts_with("rm\n-f\naf-gate-"), "{cleanup}");
        }
    }

    #[test]
    fn a_missing_runtime_is_absent() {
        let provider = ContainerProvider::with_runtime("/nonexistent/runtime");
        assert_eq!(*provider.availability(), Availability::Absent);
        assert_eq!(provider.isolation(), Isolation::None);
    }

    /// The invocation is the part a stub can prove: one bind, no network, only declared
    /// environment, and a name that can be reaped after client failure.
    #[test]
    fn the_invocation_binds_only_the_sandbox_and_disables_the_network() {
        let provider =
            ContainerProvider::with_runtime("/nonexistent/runtime").with_image("example/image:tag");
        let argv = provider.invocation(
            Path::new("/tmp/sandbox-root"),
            "/bin/sh",
            &["-c".to_string(), "make test".to_string()],
            &[
                ("LC_ALL".to_string(), "C".to_string()),
                ("TZ".to_string(), "UTC".to_string()),
            ],
            "af-gate-test",
        );
        let user = format!(
            "{}:{}",
            nix::unistd::getuid().as_raw(),
            nix::unistd::getgid().as_raw()
        );

        assert_eq!(
            argv,
            vec![
                "run",
                "--rm",
                "--name",
                "af-gate-test",
                "--network=none",
                "--env-file",
                "/dev/null",
                "--pids-limit",
                "4096",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--init",
                "--user",
                user.as_str(),
                "-e",
                "LC_ALL=C",
                "-e",
                "TZ=UTC",
                "--workdir",
                "/work",
                "--volume",
                "/tmp/sandbox-root:/work:rw",
                "example/image:tag",
                "/bin/sh",
                "-c",
                "make test",
            ]
        );

        // Exactly one bind, and it is the sandbox.
        assert_eq!(argv.iter().filter(|a| *a == "--volume").count(), 1);
        assert!(!argv.iter().any(|a| a.contains("/var/run/docker.sock")));
        assert!(!argv.iter().any(|a| a == "--privileged"));
        assert!(!argv.iter().any(|a| a.starts_with("--network=host")));
        assert!(!argv.iter().any(|a| a == "--tmpfs"));
        // No memory or CPU ceiling unless the binding names one.
        assert!(
            !argv
                .iter()
                .any(|a| a.starts_with("--memory") || a.starts_with("--cpus"))
        );
    }

    #[test]
    fn worker_runtime_refuses_unproven_or_unsupported_resource_enforcement() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        write_runtime(
            &runtime,
            "#!/bin/sh\nprintf '%s' '{\"MemoryLimit\":true,\"SwapLimit\":true,\"CpuCfsQuota\":true,\"PidsLimit\":false}'\n",
        );
        let provider = ContainerProvider::with_runtime_before(
            &runtime,
            Instant::now() + LOAD_SAFE_WALL,
            directory.path(),
        );
        assert!(
            !provider.availability().usable(),
            "an exit-zero client is not proof of required enforcement"
        );
        assert!(provider.availability().reason().contains("enforcement"));
    }

    #[test]
    fn worker_image_admission_refuses_volumes_that_shadow_declared_source() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        write_runtime(
            &runtime,
            "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nprintf '%s' '{\"/work/src\":{}}'\n",
        );
        let provider = ContainerProvider::with_runtime(&runtime);
        assert!(
            provider
                .admit_worker_image(
                    directory.path(),
                    "/bin/sh",
                    &[],
                    Instant::now() + LOAD_SAFE_WALL,
                    None
                )
                .unwrap_err()
                .contains("image-declared volumes")
        );
    }

    #[test]
    fn worker_image_admission_refuses_unsafe_mount_layout_before_source_is_mounted() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        write_runtime(
            &runtime,
            "#!/bin/sh\ncase \"$1\" in info|rm) exit 0;; image) printf null; exit 0;; run) for arg in \"$@\"; do test \"$arg\" != --volume || exit 91; done; exit 72;; esac\n",
        );
        let provider =
            ContainerProvider::with_runtime(&runtime).with_worker_layout(WorkerContainerLayout {
                package: directory.path().join("package"),
                roots: vec![],
                source_writable: true,
                base: None,
                processes: 32,
                disk_bytes: 16 * 1024 * 1024,
                probe_shell: "/bin/sh".into(),
            });
        assert!(
            provider
                .admit_worker_image(
                    directory.path(),
                    "/bin/sh",
                    &[],
                    Instant::now() + LOAD_SAFE_WALL,
                    None,
                )
                .unwrap_err()
                .contains("non-symlink image mount roots")
        );
    }

    #[test]
    fn worker_container_cleanup_confirms_an_already_removed_failure_in_its_private_context() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        write_runtime(
            &runtime,
            "#!/bin/sh\ncase \"$1\" in info) printf '%s' '{\"MemoryLimit\":true,\"SwapLimit\":true,\"CpuCfsQuota\":true,\"PidsLimit\":true}'; exit 0;; rm) exit 1;; ps) test \"$HOME\" = \"$DOCKER_CONFIG\"; exit $?;; *) printf failure; exit 7;; esac\n",
        );
        for name in ["home", "package", "source"] {
            std::fs::create_dir(directory.path().join(name)).unwrap();
        }
        let provider = ContainerProvider::with_runtime_before(
            &runtime,
            Instant::now() + LOAD_SAFE_WALL,
            &directory.path().join("home"),
        )
        .with_worker_layout(WorkerContainerLayout {
            package: directory.path().join("package"),
            roots: vec![],
            source_writable: true,
            base: None,
            processes: 32,
            disk_bytes: 16 * 1024 * 1024,
            probe_shell: "/bin/sh".into(),
        });
        let capture = provider
            .with_identity_probe_root(&directory.path().join("source"))
            .exec_worker_captured(
                &directory.path().join("source"),
                "/bin/false",
                &[],
                &[],
                vec![],
                Instant::now() + LOAD_SAFE_WALL,
                None,
            )
            .unwrap();
        assert_eq!(capture.capture.status.unwrap().code(), Some(7));
        assert!(
            capture.cleanup_confirmed,
            "an explicit empty same-context listing proves --rm already completed"
        );
    }

    #[test]
    fn worker_invocation_pins_entrypoint_private_roots_read_only_image_and_no_acquisition() {
        let provider = ContainerProvider::with_runtime("/nonexistent/runtime")
            .with_limits(Some("67108864".into()), Some("1".into()))
            .with_worker_layout(WorkerContainerLayout {
                package: "/private/package".into(),
                roots: vec![
                    ("/private/state".into(), "state".into(), true),
                    ("/private/knowledge".into(), "knowledge".into(), false),
                ],
                source_writable: false,
                base: Some("/private/base".into()),
                processes: 32,
                disk_bytes: 16 * 1024 * 1024,
                probe_shell: "/bin/sh".into(),
            });
        let argv = provider.invocation(
            Path::new("/private/source"),
            "/bin/sh",
            &["/af/package/harness.sh".into()],
            &[],
            "af-worker-test",
        );
        for value in [
            "--interactive",
            "--pull=never",
            "--read-only",
            "--network=none",
            "--log-driver=none",
            "--ipc=none",
            "/private/source:/work:ro",
            "/private/package:/af/package:ro",
            "/private/state:/af/state:ro",
            "/private/knowledge:/af/knowledge:ro",
            "/private/base:/base:ro",
        ] {
            assert!(argv.iter().any(|arg| arg == value), "missing {value}");
        }
        assert!(!argv.iter().any(|arg| arg == "--tty" || arg == "-t"));
        assert!(!argv.iter().any(|argument| argument.ends_with(":rw")));
        let mut prepared = provider.clone();
        prepared.storage_volume = Some("af-storage-private".into());
        let bounded =
            prepared.invocation(Path::new("/private/source"), "/bin/sh", &[], &[], "worker");
        for (root, target, readonly) in [
            ("source", "/work", true),
            ("state", "/af/state", false),
            ("knowledge", "/af/knowledge", true),
            ("base", "/base", true),
            ("tmp", "/tmp", false),
            ("run", "/run", false),
            ("shm", "/dev/shm", false),
            ("mqueue", "/dev/mqueue", true),
        ] {
            assert!(bounded.contains(&format!("type=volume,src=af-storage-private,dst={target},volume-subpath={root},volume-nocopy{}", if readonly { ",readonly" } else { "" })));
        }
        assert!(
            !bounded
                .iter()
                .any(|argument| argument.ends_with(":rw") || argument == "--tmpfs")
        );
        let entrypoint = argv.iter().position(|arg| arg == "--entrypoint").unwrap();
        assert_eq!(argv[entrypoint + 1], "/bin/sh");
        let image = argv.iter().position(|arg| arg == DEFAULT_IMAGE).unwrap();
        assert_eq!(&argv[image + 1..], &["/af/package/harness.sh"]);
        assert_eq!(
            argv[argv.iter().position(|arg| arg == "--pids-limit").unwrap() + 1],
            "32"
        );
        let image_only = provider.clone().without_worker_mounts().invocation(
            Path::new("/private/source"),
            "/bin/sh",
            &["-c".into(), "test ! -L /work".into()],
            &[],
            "af-worker-image-test",
        );
        assert!(!image_only.iter().any(|argument| argument == "--volume"));
        assert_eq!(
            image_only[image_only
                .iter()
                .position(|arg| arg == "--workdir")
                .unwrap()
                + 1],
            "/"
        );
        let readonly = provider
            .with_identity_probe_root(Path::new("/empty"))
            .invocation(Path::new("/empty"), "/bin/sh", &[], &[], "af-worker-test");
        assert!(readonly.contains(&"/empty:/work:ro".into()));
        assert!(readonly.contains(&"/empty:/af/package:ro".into()));
        assert!(
            !readonly
                .iter()
                .any(|argument| argument.contains("/private/state")
                    || argument.contains("/private/base"))
        );
    }

    /// Memory (swap included) and CPU ceilings appear exactly when the Gate binding names them,
    /// before the image, and change nothing else: the digest-pinned image, `--network=none`
    /// and the single bind stay as they are.
    #[test]
    fn configured_limits_add_memory_and_cpu_ceilings_and_nothing_else() {
        let image = DEFAULT_IMAGE;
        let plain = ContainerProvider::with_runtime("/nonexistent/runtime");
        let argv = |provider: &ContainerProvider| {
            provider.invocation(
                Path::new("/tmp/sandbox-root"),
                "/bin/true",
                &[],
                &[],
                "af-gate-test",
            )
        };
        let base = argv(&plain);
        let flag_value = |argv: &[String], flag: &str| {
            argv.iter()
                .position(|a| a == flag)
                .map(|index| argv[index + 1].clone())
        };
        for (memory, cpus) in [
            (Some("8g"), Some("1.5")),
            (Some("512m"), None),
            (None, Some("4")),
        ] {
            let limited = argv(
                &plain
                    .clone()
                    .with_limits(memory.map(str::to_owned), cpus.map(str::to_owned)),
            );
            assert_eq!(flag_value(&limited, "--memory").as_deref(), memory);
            assert_eq!(flag_value(&limited, "--memory-swap").as_deref(), memory);
            assert_eq!(flag_value(&limited, "--cpus").as_deref(), cpus);
            let image_at = limited.iter().position(|a| a == image).unwrap();
            for flag in ["--memory", "--memory-swap", "--cpus"] {
                if let Some(at) = limited.iter().position(|a| a == flag) {
                    assert!(at < image_at, "{flag} after the image");
                }
            }
            let rest: Vec<&String> = limited
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    !["--memory", "--memory-swap", "--cpus"].iter().any(|flag| {
                        limited[*index] == *flag || (*index > 0 && limited[*index - 1] == *flag)
                    })
                })
                .map(|(_, a)| a)
                .collect();
            assert_eq!(rest, base.iter().collect::<Vec<_>>());
            assert!(limited.contains(&"--network=none".to_string()));
            assert_eq!(limited.iter().filter(|a| *a == "--volume").count(), 1);
            assert!(image.contains("@sha256:"));
        }
    }

    /// A container command that writes past the output cap is an execution failure naming the
    /// cap, never a passing check on a prefix (M2).
    #[test]
    fn container_output_past_the_cap_fails_the_execution() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("runtime");
        write_runtime(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nif [ \"$1\" = rm ]; then exit 0; fi\nhead -c 10000 /dev/zero\n",
        );
        let provider = ContainerProvider::with_runtime(&fake).with_output_cap(4096);
        let error = provider
            .exec_evidenced_controlled(dir.path(), "/bin/true", &[], &[], LOAD_SAFE_WALL, None)
            .unwrap_err();
        assert!(error.cleanup_confirmed(), "{error}");
        assert!(
            error
                .to_string()
                .contains("stdout was truncated at the 4 KiB output cap: it wrote 10000 bytes"),
            "{error}"
        );
        let within = ContainerProvider::with_runtime(&fake)
            .with_output_cap(16384)
            .exec_evidenced_controlled(dir.path(), "/bin/true", &[], &[], LOAD_SAFE_WALL, None)
            .unwrap();
        assert_eq!(within.output.stdout.len(), 10000);
    }

    /// The container command's `af.process` span ends after the cap judgement: a command that
    /// writes past its cap and exits 0 fails its execution naming the cap, and its span ends
    /// with `error.type = "failed"` and the cap's reason, the exit and the kept stream sizes
    /// still those of the capture.
    #[test]
    fn container_output_past_the_cap_ends_its_span_failed() {
        use af_telemetry::level::Level;
        use af_telemetry::record::SealExit;
        use af_telemetry::segment::writer::{Writer, WriterOptions};
        use serde_json::Value;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("runtime");
        write_runtime(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nif [ \"$1\" = rm ]; then exit 0; fi\nhead -c 10000 /dev/zero\nexit 0\n",
        );
        let reason = "stdout was truncated at the 4 KiB output cap: it wrote 10000 bytes, of \
                      which only the first 4 KiB were kept";
        let writer = Writer::new(WriterOptions::new(
            dir.path().join("telemetry"),
            Level::Spans,
        ))
        .unwrap();
        let dispatch = tracing::Dispatch::new(af_telemetry::subscriber::TelemetrySubscriber::new(
            writer.clone(),
        ));
        tracing::dispatcher::with_default(&dispatch, || {
            let run = tracing::info_span!("af.run", af.run_id = "run-1");
            let _run = run.enter();
            let error = ContainerProvider::with_runtime(&fake)
                .with_output_cap(4096)
                .exec_evidenced_controlled(dir.path(), "/bin/true", &[], &[], LOAD_SAFE_WALL, None)
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("container command {reason}")),
                "{error}"
            );
        });
        writer.flush();
        let text = std::fs::read_to_string(writer.path().unwrap()).unwrap();
        writer.finish(SealExit::Ok).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let start = lines
            .iter()
            .find(|line| {
                line["t"] == "span_start"
                    && line["attributes"]["af.process.kind"] == "container command"
            })
            .unwrap_or_else(|| panic!("no container command span: {text}"));
        let end = lines
            .iter()
            .find(|line| line["t"] == "span_end" && line["span_id"] == start["span_id"])
            .unwrap_or_else(|| panic!("no end for {start}"));
        let attributes = &end["attributes"];
        assert_eq!(end["status"], "error", "{end}");
        assert_eq!(attributes["error.type"], "failed", "{end}");
        assert_eq!(attributes["af.failure.class"], "failed", "{end}");
        assert_eq!(attributes["af.failure.reason"], reason, "{end}");
        assert_eq!(attributes["process.exit_code"], 0, "{end}");
        assert_eq!(attributes["af.stdout_bytes"], 4096, "{end}");
        assert_eq!(attributes["af.stderr_bytes"], 0, "{end}");
        assert!(attributes["process.pid"].is_u64(), "{end}");
    }

    #[test]
    fn a_check_runtime_adds_container_mounts_and_keeps_the_single_bind() {
        let provider = ContainerProvider::with_runtime("/nonexistent/runtime")
            .with_image("example/image:tag")
            .with_check_runtime();
        let argv = provider.invocation(
            Path::new("/tmp/sandbox-root"),
            "/bin/true",
            &[],
            &crate::CheckRuntime::container_environment(),
            "af-gate-test",
        );
        let mounts: Vec<&String> = argv
            .iter()
            .zip(argv.iter().skip(1))
            .filter(|(flag, _)| *flag == "--tmpfs")
            .map(|(_, value)| value)
            .collect();
        assert_eq!(
            mounts,
            [
                "/af-check/home:rw,exec,mode=1777",
                "/af-check/tmp:rw,exec,mode=1777",
                "/af-check/scratch:rw,exec,mode=1777",
                "/af-check/cache:rw,exec,mode=1777",
            ]
        );
        assert!(argv.contains(&"HOME=/af-check/home".to_string()));
        assert_eq!(argv.iter().filter(|a| *a == "--volume").count(), 1);
        let image = argv.iter().position(|a| a == "example/image:tag").unwrap();
        assert!(argv.iter().position(|a| a == "--tmpfs").unwrap() < image);
    }

    #[test]
    #[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
    fn a_timed_out_container_is_removed_before_execution_returns() {
        let provider = ContainerProvider::detect();
        let runtime = match provider.availability() {
            Availability::Usable { runtime } => runtime.clone(),
            unavailable => panic!(
                "this probe was invoked explicitly and needs a live runtime: {}",
                unavailable.reason()
            ),
        };
        let sandbox = tempfile::tempdir().unwrap();
        let identity = tempfile::Builder::new()
            .prefix("af-gate-timeout-probe-")
            .tempdir()
            .unwrap();
        let execution_name = identity.path().file_name().unwrap().to_str().unwrap();

        let error = provider
            .exec_evidenced_named(
                &runtime,
                sandbox.path(),
                "/bin/sleep",
                &["60".to_string()],
                &[],
                Duration::from_millis(250),
                execution_name,
            )
            .unwrap_err();
        assert!(error.cleanup_confirmed(), "{error}");
        assert!(
            error
                .to_string()
                .contains("container command did not finish"),
            "{error}"
        );

        let mut inspect = std::process::Command::new(&runtime);
        inspect.args(["inspect", execution_name]);
        let inspect = run_bounded(inspect, PROBE_TIMEOUT, "post-timeout container inspect")
            .expect("live runtime remains responsive");
        assert!(
            !inspect.status.success(),
            "timed-out container {execution_name} still exists"
        );
    }
}
