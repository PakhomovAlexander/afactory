//! Explicit application dependencies. The library never discovers machine configuration.

use std::path::{Path, PathBuf};

/// Machine services used only by the Project operation the caller requests. Implementations
/// preserve lazy, best-effort Store registration and the application's path/configuration rules.
/// Git itself uses `af_process` and the isolated command in `worktree_git`, never a shell or a frontend.
pub trait Host {
    fn state_root(&self) -> Result<PathBuf, String>;
    fn record_project_store(&self, directory: &Path);
    fn default_task_store(&self, checkout: &Path) -> Result<PathBuf, String>;
    fn resolve_path(&self, path: &Path) -> Result<PathBuf, String>;
    fn valid_project_declaration(&self, text: &str) -> bool;
    fn configuration_table(
        &self,
        checkout: &Path,
        key: &str,
    ) -> Result<Option<toml::Value>, String>;
    fn warning(&self, message: &str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Checkout, Registration, adopt, brief, project_id, store_directory, survey};
    use std::cell::RefCell;

    struct Machine {
        state: PathBuf,
        registrations: RefCell<Vec<PathBuf>>,
        valid_declaration: bool,
        policy: Result<toml::Value, String>,
    }

    impl Host for Machine {
        fn state_root(&self) -> Result<PathBuf, String> {
            Ok(self.state.clone())
        }
        fn record_project_store(&self, directory: &Path) {
            self.registrations
                .borrow_mut()
                .push(directory.to_path_buf());
        }
        fn default_task_store(&self, checkout: &Path) -> Result<PathBuf, String> {
            assert!(checkout.is_absolute());
            Ok(self.state.join("explicit-task-store"))
        }
        fn resolve_path(&self, path: &Path) -> Result<PathBuf, String> {
            assert!(path.is_absolute());
            Ok(path.to_path_buf())
        }
        fn valid_project_declaration(&self, _: &str) -> bool {
            self.valid_declaration
        }
        fn configuration_table(&self, _: &Path, key: &str) -> Result<Option<toml::Value>, String> {
            self.policy
                .as_ref()
                .map(|value| value.get(key).cloned())
                .map_err(Clone::clone)
        }
        fn warning(&self, _: &str) {
            panic!("no warning expected");
        }
    }

    #[test]
    fn registration_uses_only_injected_machine_paths_registry_and_declaration_validation() {
        let root = tempfile::tempdir().unwrap();
        let checkout_path = root.path().join("checkout");
        std::fs::create_dir_all(checkout_path.join(".af")).unwrap();
        std::fs::write(
            checkout_path.join(".af/af.toml"),
            "[project]\nname = 'fixture'\nmin_af = '0.15'\n",
        )
        .unwrap();
        let checkout = Checkout {
            toplevel: checkout_path,
            repository_id: "1".repeat(40),
            project_id: project_id(&"1".repeat(40)),
        };
        // The same bytes are accepted or excluded only by the application's policy validator.
        for valid in [true, false] {
            let host = Machine {
                state: root.path().join(if valid { "accepted" } else { "refused" }),
                registrations: RefCell::default(),
                valid_declaration: valid,
                policy: Ok(toml::Table::new().into()),
            };
            let (store, registration) = adopt(&host, &checkout, None).unwrap();
            assert!(matches!(registration, Registration::New(_)));
            let project = store.log().unwrap().project.unwrap();
            assert_eq!(project.name.as_deref(), valid.then_some("fixture"));
            assert_eq!(project.min_af.as_deref(), valid.then_some("0.15"));
            assert_eq!(
                store.directory,
                store_directory(&host.state, &checkout.project_id)
            );
            assert_eq!(*host.registrations.borrow(), [store.directory.clone()]);
            assert_eq!(
                store.paths().unwrap()[0].state,
                host.state.join("explicit-task-store")
            );
            drop(store);
            let (store, registration) = adopt(&host, &checkout, None).unwrap();
            assert!(matches!(registration, Registration::Existing(_)));
            assert_eq!(store.log().unwrap().workspaces.len(), 1);
            assert_eq!(host.registrations.borrow().len(), 2);
        }
        assert!(!checkout.toplevel.join("workspaces.toml").exists());
    }

    #[test]
    fn survey_and_brief_policy_use_the_injected_configuration_and_keep_refusal_words() {
        let mut host = Machine {
            state: PathBuf::from("/unused"),
            registrations: RefCell::default(),
            valid_declaration: false,
            policy: Ok(toml::from_str(
                "[survey]\nmax_commits = 17\n[brief.orient]\nmax_bytes = 4096\n",
            )
            .unwrap()),
        };
        let checkout = Path::new("/unused-checkout");
        assert_eq!(
            survey::policy::Policy::load(&host, checkout)
                .unwrap()
                .max_commits,
            17
        );
        assert_eq!(
            brief::load_policy(&host, checkout).unwrap()["orient"]
                .max_bytes
                .get(),
            4096
        );
        host.policy = Err("private machine configuration diagnostic".into());
        let expected = "the configuration does not load; `af config show --origin` names the file";
        assert_eq!(
            survey::policy::Policy::load(&host, checkout).unwrap_err(),
            expected
        );
        assert_eq!(brief::load_policy(&host, checkout).unwrap_err(), expected);
        assert!(host.registrations.borrow().is_empty());
    }
}
