//! A check's runtime directory: the one writable place outside its tree that af owns for it.
//!
//! Every check af runs — a Task code check, a measurement check, a review gate check and a
//! post-apply integration check — gets a `HOME`, a `TMPDIR`, an `AF_CHECK_SCRATCH` and an
//! `XDG_CACHE_HOME` below one directory named `af-check-<pid>-<random>` in the process's
//! temporary root. The four are created empty and private before the check and removed with
//! the whole directory when the check ends, without following a link, so a check never needs
//! a path outside them and never leaves one behind. A process killed mid-check leaves the
//! directory to the crash sweep of [`crate::stale`], which knows the name.
//!
//! A container check sees the same four variables, pointed at `tmpfs` mounts inside the
//! container instead of the host directory: the container keeps its single bind (the sandbox),
//! and the mounts go with the container.

use std::path::{Path, PathBuf};

use crate::stale;

/// The variables a check receives, each with the directory below the runtime it names.
const LAYOUT: [(&str, &str); 4] = [
    ("HOME", "home"),
    ("TMPDIR", "tmp"),
    ("AF_CHECK_SCRATCH", "scratch"),
    ("XDG_CACHE_HOME", "cache"),
];

/// Where a container check's runtime directories are mounted, inside the container.
pub const CONTAINER_CHECK_RUNTIME: &str = "/af-check";

/// One check's runtime directory, removed when dropped.
#[derive(Debug)]
pub struct CheckRuntime {
    path: PathBuf,
}

impl CheckRuntime {
    /// Create `af-check-<pid>-<random>` under the temporary root with its four directories,
    /// each empty and private.
    pub fn new() -> std::io::Result<Self> {
        let directory = stale::check_tempdir()?;
        // From here the runtime owns removal; a failure below removes what was made.
        let runtime = Self {
            path: directory.keep(),
        };
        for (_, name) in LAYOUT {
            std::fs::create_dir(runtime.path.join(name))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    runtime.path.join(name),
                    std::fs::Permissions::from_mode(0o700),
                )?;
            }
        }
        Ok(runtime)
    }

    /// The runtime directory itself. A caller may put its own private directories here (a
    /// toolchain copy, a cold target); they are removed with the runtime.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `HOME`, `TMPDIR`, `AF_CHECK_SCRATCH` and `XDG_CACHE_HOME`, each an absolute path below
    /// [`Self::path`], in that order.
    pub fn environment(&self) -> Vec<(String, String)> {
        LAYOUT
            .iter()
            .map(|(variable, name)| {
                (
                    (*variable).to_string(),
                    self.path.join(name).display().to_string(),
                )
            })
            .collect()
    }

    /// The same four variables as a container check sees them: below
    /// [`CONTAINER_CHECK_RUNTIME`], each a `tmpfs` mount the container provider adds when it
    /// runs a check (see [`crate::ContainerProvider::with_check_runtime`]).
    pub fn container_environment() -> Vec<(String, String)> {
        LAYOUT
            .iter()
            .map(|(variable, name)| {
                (
                    (*variable).to_string(),
                    format!("{CONTAINER_CHECK_RUNTIME}/{name}"),
                )
            })
            .collect()
    }

    /// The container mount points of [`Self::container_environment`].
    pub(crate) fn container_mounts() -> Vec<String> {
        LAYOUT
            .iter()
            .map(|(_, name)| format!("{CONTAINER_CHECK_RUNTIME}/{name}"))
            .collect()
    }
}

impl Drop for CheckRuntime {
    fn drop(&mut self) {
        let (Some(parent), Some(name)) = (self.path.parent(), self.path.file_name()) else {
            return;
        };
        if !stale::remove_tree_nofollow(parent, name) {
            // Whatever is left keeps its `af-check-<pid>-` name; the next crash sweep after this
            // process ends removes it.
            eprintln!(
                "af: could not remove the check runtime {}; a later run removes it",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_names_four_private_writable_directories_inside_itself() {
        let runtime = CheckRuntime::new().unwrap();
        let root = runtime.path().to_path_buf();
        let name = root.file_name().unwrap().to_str().unwrap().to_string();
        assert!(
            name.starts_with(&format!("af-check-{}-", std::process::id())),
            "{name}"
        );
        let environment = runtime.environment();
        assert_eq!(
            environment
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["HOME", "TMPDIR", "AF_CHECK_SCRATCH", "XDG_CACHE_HOME"]
        );
        for (_, value) in &environment {
            let path = Path::new(value);
            assert!(path.starts_with(&root), "{value}");
            assert!(path.is_dir(), "{value}");
            assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
            std::fs::write(path.join("probe"), b"x").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o700, "{value}");
            }
        }
        drop(runtime);
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_runtime_removes_read_only_content_and_never_follows_a_link() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("precious"), b"keep").unwrap();
        let runtime = CheckRuntime::new().unwrap();
        let root = runtime.path().to_path_buf();
        let nested = root.join("scratch").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("file"), b"x").unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o555)).unwrap();
        symlink(outside.path(), root.join("home").join("escape")).unwrap();
        drop(runtime);
        assert!(!root.exists());
        assert!(outside.path().join("precious").exists());
    }

    #[test]
    fn a_container_check_sees_the_same_variables_below_its_mounts() {
        let environment = CheckRuntime::container_environment();
        assert_eq!(environment[0], ("HOME".into(), "/af-check/home".into()));
        assert_eq!(
            environment
                .iter()
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>(),
            CheckRuntime::container_mounts()
        );
    }
}
