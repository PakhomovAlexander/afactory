//! Where the browser was started. Inside a repository it is the project scope; anywhere else it
//! is the user scope. Resolution is `config::load`, so the browser and `af config` never
//! disagree about the toplevel.

use std::path::{Path, PathBuf};

use crate::config::{self, Config};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    User,
    Project,
}

pub(crate) struct Scope {
    pub(crate) kind: ScopeKind,
    /// The repository toplevel, or the home directory for the user scope.
    pub(crate) root: PathBuf,
    /// Every layer `af config show` reads here; the machine-owned layers only for the user
    /// scope.
    pub(crate) config: Config,
    /// The directory paths are abbreviated against as `~`.
    pub(crate) home: Option<PathBuf>,
    /// The machine-local Provider registry `af provider` reads.
    pub(crate) registry: Option<PathBuf>,
}

impl Scope {
    /// `dir`, or the current directory, inside a repository is that project's scope; anywhere
    /// else is the user scope.
    pub(crate) fn resolve(dir: Option<&Path>) -> Result<Scope, String> {
        // Outside a repository the user scope reads the machine-owned layers only, so a
        // directory layer above the start, valid or not, is never opened for it.
        let start = match dir {
            Some(dir) => dir.to_path_buf(),
            None => {
                std::env::current_dir().map_err(|error| format!("current directory: {error}"))?
            }
        };
        if config::git_toplevel(&start).is_none() {
            return Scope::user();
        }
        let config = config::load(dir)?;
        match config.toplevel.clone() {
            Some(toplevel) => Ok(Scope::project(toplevel, config)),
            None => Scope::user(),
        }
    }

    pub(crate) fn user() -> Result<Scope, String> {
        let home = config::home()?;
        Ok(Scope {
            kind: ScopeKind::User,
            root: home.clone(),
            config: config::load_machine()?,
            home: Some(home),
            registry: crate::providers::registry_location(),
        })
    }

    pub(crate) fn project(toplevel: PathBuf, config: Config) -> Scope {
        Scope {
            kind: ScopeKind::Project,
            root: toplevel,
            config,
            home: config::home().ok(),
            registry: crate::providers::registry_location(),
        }
    }

    /// The same scope read from disk again.
    pub(crate) fn reload(&self) -> Result<Scope, String> {
        match self.kind {
            ScopeKind::User => Scope::user(),
            ScopeKind::Project => Scope::resolve(Some(self.root.as_path())),
        }
    }

    pub(crate) fn toplevel(&self) -> Option<&Path> {
        (self.kind == ScopeKind::Project).then_some(self.root.as_path())
    }

    /// The scope as the bar and the status line name it: the toplevel's basename, or `~`.
    pub(crate) fn name(&self) -> String {
        match (self.kind, self.root.file_name()) {
            (ScopeKind::Project, Some(name)) => name.to_string_lossy().into_owned(),
            (ScopeKind::Project, None) => self.root.display().to_string(),
            (ScopeKind::User, _) => "~".to_owned(),
        }
    }

    pub(crate) fn word(&self) -> &'static str {
        match self.kind {
            ScopeKind::User => "user",
            ScopeKind::Project => "project",
        }
    }

    /// A path as the panes show it: repository-relative inside the project, `~/...` under the
    /// home directory, absolute otherwise.
    pub(crate) fn display(&self, path: &Path) -> String {
        if let Some(toplevel) = self.toplevel()
            && let Ok(relative) = path.strip_prefix(toplevel)
            && !relative.as_os_str().is_empty()
        {
            return relative.display().to_string();
        }
        self.abbreviate(path)
    }

    /// A path with the home directory written `~`.
    pub(crate) fn abbreviate(&self, path: &Path) -> String {
        let home = self.home.as_deref();
        let relative = home.and_then(|home| path.strip_prefix(home).ok());
        match relative {
            Some(relative) if relative.as_os_str().is_empty() => "~".to_owned(),
            Some(relative) => format!("~/{}", relative.display()),
            None => path.display().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_directory_layer_above_a_plain_directory_does_not_block_the_user_scope() {
        let temp = tempfile::tempdir().unwrap();
        let above = temp.path().join("above");
        let plain = above.join("plain");
        std::fs::create_dir_all(plain.join("deeper")).unwrap();
        std::fs::create_dir_all(above.join(".af")).unwrap();
        std::fs::write(above.join(".af/af.toml"), "this = is not [toml\n").unwrap();
        let scope = Scope::resolve(Some(&plain.join("deeper"))).unwrap();
        assert_eq!(scope.kind, ScopeKind::User);
        assert!(scope.toplevel().is_none());
    }
}
