//! Git-shared catalogs describe immutable packages, never credentials or host configuration.
use super::catalog::TaskPackagePin;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogPathBase {
    #[default]
    Repository,
    Manifest,
}

/// Package names may be prefixes of other names. Separate physical roots prevent a parent's
/// digest from accidentally including a nested package's files after export or vendoring.
pub fn package_directory(name: &str) -> String {
    let digest = review_store::canonical::blob_content_id(name.as_bytes());
    format!("packages/{}", digest.trim_start_matches("sha256:"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedTaskCatalog {
    pub schema: String,
    pub packages: BTreeMap<String, TaskPackagePin>,
    /// Existing catalogs use repository-relative paths. Export bundles opt into manifest-
    /// relative paths so the whole directory can move without rewriting package pins.
    #[serde(default, skip_serializing_if = "CatalogPathBase::is_repository")]
    pub path_base: CatalogPathBase,
    /// Catalog files in the same exact Git commit. Another repository is a separate explicit
    /// sync; imported text cannot initiate a new transport or read another local repository.
    #[serde(default)]
    pub imports: BTreeSet<String>,
}

impl CatalogPathBase {
    fn is_repository(&self) -> bool {
        *self == Self::Repository
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCatalogImportLock {
    pub schema: String,
    pub source: CatalogSourceLock,
    pub catalogs: BTreeMap<String, String>,
    /// Relative to this lock's directory, with all transitive package bytes vendored beside it.
    pub packages: BTreeMap<String, TaskPackagePin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSourceLock {
    /// Hash of the explicit sync source. Machine paths and private remote URLs are not exported.
    pub source_id: String,
    pub requested_revision: String,
    pub commit: String,
}

pub fn safe_relative_path(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && !matches!(part, "." | ".." | ".git"))
}

impl SharedTaskCatalog {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.shared-task-catalog/1"
            || self.packages.len() > 128
            || self.imports.len() > 32
            || self.imports.iter().any(|p| !safe_relative_path(p))
            || self.packages.iter().any(|(name, pin)| {
                !review_core::task::is_package_name(name)
                    || name.starts_with("local/")
                    || !safe_relative_path(&pin.path)
            })
        {
            return Err(
                "Shared catalog requires bounded names, relative paths and no local packages"
                    .into(),
            );
        }
        Ok(())
    }
}

impl TaskCatalogImportLock {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.task-catalog-import/1"
            || !review_core::is_digest(&self.source.source_id)
            || !matches!(self.source.commit.len(), 40 | 64)
            || !self.source.commit.bytes().all(|b| b.is_ascii_hexdigit())
            || self.source.requested_revision.is_empty()
            || self.source.requested_revision.len() > 1024
            || self.source.requested_revision.chars().any(char::is_control)
            || self.catalogs.is_empty()
            || self.catalogs.len() > 32
            || self.packages.is_empty()
            || self.packages.len() > 128
            || self
                .catalogs
                .iter()
                .any(|(path, id)| !safe_relative_path(path) || !review_core::is_digest(id))
            || self.packages.iter().any(|(name, pin)| {
                !review_core::task::is_package_name(name)
                    || name.starts_with("local/")
                    || !safe_relative_path(&pin.path)
                    || !review_core::is_digest(&pin.digest)
            })
        {
            return Err("Imported catalog lacks exact bounded Git and package identities".into());
        }
        Ok(())
    }
}
