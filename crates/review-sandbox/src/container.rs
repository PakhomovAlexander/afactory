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
//! The invocation this builds — `--network=none`, a single bind of the sandbox, no inherited
//! environment — is asserted exactly, using a recording stub in place of the runtime. That proves
//! the *plumbing*: the right flags, the right mount, nothing extra.
//!
//! It does **not** prove containment. Only a real runtime can do that, and
//! `tests/container_probes.rs` does exactly that — the `malicious-check.md` probes that need
//! isolation, run against a live daemon locally and in CI, each paired with a control proving
//! the container genuinely runs work.

use std::path::{Path, PathBuf};
use std::time::Duration;

use review_process::{SupervisedError, run_supervised};

use crate::{Isolation, Mode, Sandbox, SandboxTemplate};

/// Runtimes tried in order. Docker first only because it is the likeliest to be present.
const RUNTIMES: [&str; 3] = ["docker", "podman", "nerdctl"];

/// Capability detection runs in every full verification gate. A wedged daemon is unavailable,
/// not authority to keep the gate open forever.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// A timed-out runtime client is not the workload: the daemon-owned container must be stopped
/// before its writable bind can be sealed. Cleanup is bounded too, so a wedged daemon cannot
/// replace the check deadline with an unbounded second wait.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

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
            availability: Self::probe(),
            image: DEFAULT_IMAGE.to_string(),
        }
    }

    /// Point at a specific runtime binary, probing it the same way. Used by tests to drive both
    /// branches without depending on what happens to be installed.
    pub fn with_runtime(path: impl AsRef<Path>) -> ContainerProvider {
        let path = path.as_ref().to_path_buf();
        ContainerProvider {
            availability: Self::probe_one(&path),
            image: DEFAULT_IMAGE.to_string(),
        }
    }

    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    fn probe() -> Availability {
        let mut first_unusable = None;
        for name in RUNTIMES {
            let Ok(path) = which(name) else { continue };
            match Self::probe_one(&path) {
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
    /// no network, only declared environment, a reaping identity, and the command in a value
    /// position after the image.
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
            // No undeclared network. This is the probe malicious-check.md cannot otherwise close.
            "--network=none".to_string(),
            // No ambient host environment crosses in. Only the kernel-owned allowlist below is
            // reintroduced explicitly.
            "--env-file".to_string(),
            "/dev/null".to_string(),
        ];
        for (key, value) in environment {
            argv.push("-e".to_string());
            argv.push(format!("{key}={value}"));
        }
        argv.extend([
            "--workdir".to_string(),
            "/work".to_string(),
            "--volume".to_string(),
            format!("{}:/work:rw", sandbox_root.display()),
            self.image.clone(),
            program.to_string(),
        ]);
        argv.extend(args.iter().cloned());
        argv
    }

    /// Run a command in the sandbox under the caller's policy deadline. Refuses when the runtime
    /// is not usable — never falls back to running it on the host, which would be containment
    /// silently becoming none.
    pub fn exec(
        &self,
        sandbox_root: &Path,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<std::process::Output, String> {
        self.exec_evidenced(sandbox_root, program, args, &[], timeout)
            .map(|execution| execution.output)
            .map_err(|error| error.to_string())
    }

    /// Execute while preserving process-supervision evidence needed by CheckResult. In
    /// particular, a descendant that keeps stderr open must remain `not_run`, not turn into a
    /// passing check merely because the container runtime's leader exited.
    pub fn exec_evidenced(
        &self,
        sandbox_root: &Path,
        program: &str,
        args: &[String],
        environment: &[(String, String)],
        timeout: Duration,
    ) -> Result<ContainerExecution, ContainerExecutionError> {
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
        self.exec_evidenced_named(
            runtime,
            sandbox_root,
            program,
            args,
            environment,
            timeout,
            execution_name,
        )
    }

    #[allow(clippy::too_many_arguments)]
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
        let mut command = std::process::Command::new(runtime);
        command.args(self.invocation(sandbox_root, program, args, environment, execution_name));
        match run_bounded_evidenced(command, timeout, "container command") {
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

fn remove_container(runtime: &Path, execution_name: &str) -> Result<(), String> {
    let mut command = std::process::Command::new(runtime);
    command.args(["rm", "-f", execution_name]);
    let execution = run_bounded_evidenced(command, CLEANUP_TIMEOUT, "container cleanup")
        .map_err(|error| error.to_string())?;
    if execution.output.status.success() {
        return Ok(());
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
    run_supervised(&mut command, None, timeout)
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
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'Cannot connect to the daemon' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let provider = ContainerProvider::with_runtime(&fake);
        assert!(matches!(
            provider.availability(),
            Availability::Unusable { .. }
        ));
        assert_eq!(provider.isolation(), Isolation::None);
        assert!(provider.availability().reason().contains("daemon"));

        // And it refuses to run rather than falling back to the host.
        let err = provider
            .exec(
                dir.path(),
                "/bin/sh",
                &["-c".into(), "echo pwned".into()],
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(
            err.starts_with("refusing to run outside a container"),
            "{err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_wedged_runtime_is_bounded_and_unusable() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("wedged-runtime");
        std::fs::write(&fake, "#!/bin/sh\nsleep 60\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = Instant::now();
        let availability =
            ContainerProvider::probe_one_with_timeout(&fake, Duration::from_millis(100));

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(availability, Availability::Unusable { .. }));
        assert!(availability.reason().contains("did not finish"));
    }

    #[test]
    #[cfg(unix)]
    fn a_wedged_container_execution_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("runtime");
        std::fs::write(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nif [ \"$1\" = rm ]; then printf '%s\\n' \"$@\" > \"$0.cleanup\"; exit 0; fi\nsleep 60\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provider = ContainerProvider::with_runtime(&fake);

        let started = Instant::now();
        let error = provider
            .exec(dir.path(), "/bin/true", &[], Duration::from_millis(100))
            .unwrap_err();
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
    #[cfg(unix)]
    fn a_failed_reap_is_distinct_from_a_safely_stopped_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("runtime");
        std::fs::write(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nif [ \"$1\" = rm ]; then echo 'daemon lost' >&2; exit 1; fi\nsleep 60\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provider = ContainerProvider::with_runtime(&fake);

        let error = provider
            .exec_evidenced(
                dir.path(),
                "/bin/true",
                &[],
                &[],
                Duration::from_millis(100),
            )
            .unwrap_err();
        assert!(!error.cleanup_confirmed(), "{error}");
        assert!(error.to_string().contains("was not confirmed"), "{error}");
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
