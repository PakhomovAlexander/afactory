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

use crate::Isolation;

/// Runtimes tried in order. Docker first only because it is the likeliest to be present.
const RUNTIMES: [&str; 3] = ["docker", "podman", "nerdctl"];

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

pub struct ContainerProvider {
    availability: Availability,
    image: String,
}

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
        if !path.exists() {
            return Availability::Absent;
        }
        let output = std::process::Command::new(path)
            .arg("info")
            .stdin(std::process::Stdio::null())
            .output();
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

    /// The exact argv for running a command inside the sandbox.
    ///
    /// Built even when the runtime is unusable, because it is the part worth asserting: one bind,
    /// no network, no inherited environment, and the command in a value position after the image.
    pub fn invocation(&self, sandbox_root: &Path, program: &str, args: &[String]) -> Vec<String> {
        let mut argv = vec![
            "run".to_string(),
            "--rm".to_string(),
            // No undeclared network. This is the probe malicious-check.md cannot otherwise close.
            "--network=none".to_string(),
            // Nothing of the host's environment crosses in.
            "--env-file".to_string(),
            "/dev/null".to_string(),
            "--workdir".to_string(),
            "/work".to_string(),
            "--volume".to_string(),
            format!("{}:/work:rw", sandbox_root.display()),
            self.image.clone(),
            program.to_string(),
        ];
        argv.extend(args.iter().cloned());
        argv
    }

    /// Run a command in the sandbox. Refuses when the runtime is not usable — never falls back
    /// to running it on the host, which would be containment silently becoming none.
    pub fn exec(
        &self,
        sandbox_root: &Path,
        program: &str,
        args: &[String],
    ) -> Result<std::process::Output, String> {
        let Availability::Usable { runtime } = &self.availability else {
            return Err(format!(
                "refusing to run outside a container: {}",
                self.availability.reason()
            ));
        };
        std::process::Command::new(runtime)
            .args(self.invocation(sandbox_root, program, args))
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| e.to_string())
    }
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
            .exec(dir.path(), "/bin/sh", &["-c".into(), "echo pwned".into()])
            .unwrap_err();
        assert!(
            err.starts_with("refusing to run outside a container"),
            "{err}"
        );
    }

    #[test]
    fn a_missing_runtime_is_absent() {
        let provider = ContainerProvider::with_runtime("/nonexistent/runtime");
        assert_eq!(*provider.availability(), Availability::Absent);
        assert_eq!(provider.isolation(), Isolation::None);
    }

    /// The invocation is the part a stub can prove: one bind, no network, no environment.
    #[test]
    fn the_invocation_binds_only_the_sandbox_and_disables_the_network() {
        let provider =
            ContainerProvider::with_runtime("/nonexistent/runtime").with_image("example/image:tag");
        let argv = provider.invocation(
            Path::new("/tmp/sandbox-root"),
            "/bin/sh",
            &["-c".to_string(), "make test".to_string()],
        );

        assert_eq!(
            argv,
            vec![
                "run",
                "--rm",
                "--network=none",
                "--env-file",
                "/dev/null",
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
}
