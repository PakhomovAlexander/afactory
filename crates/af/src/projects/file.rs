//! `workspaces.toml`: the host paths of a Project's Workspaces on this machine.
//!
//! Machine-written TOML beside the Project Store's log, outside every record, like
//! `stores.toml` (ADR-0144): the checkout, the Task Store and, when the Workspace has them, the
//! bindings file and the Task-file directory of each Workspace, keyed by its ID. It is replaced
//! whole through a temporary file and a rename, under the Project Store's lock, so a reader never
//! sees half of it.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;

/// One Workspace's paths on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspacePaths {
    pub(crate) id: String,
    pub(crate) label: String,
    /// The checkout.
    pub(crate) path: PathBuf,
    /// Its Task Store.
    pub(crate) state: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bindings: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tasks: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspacesFile {
    version: u32,
    #[serde(default)]
    workspaces: Vec<WorkspacePaths>,
}

/// Where a Project Store keeps the file.
pub(super) fn location(store: &Path) -> PathBuf {
    store.join("workspaces.toml")
}

/// Every Workspace's paths, in label order. A missing file is empty.
pub(super) fn read(location: &Path) -> Result<Vec<WorkspacePaths>, String> {
    match std::fs::read_to_string(location) {
        Ok(text) => parse(&text).map_err(|error| {
            format!(
                "{}: {error}; remove the file and run `af project show` in each checkout to \
                 register it again",
                location.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("{}: {error}", location.display())),
    }
}

pub(super) fn parse(text: &str) -> Result<Vec<WorkspacePaths>, String> {
    let file: WorkspacesFile = toml::from_str(text).map_err(|error| error.to_string())?;
    if file.version != VERSION {
        return Err(format!("version {} is not {VERSION}", file.version));
    }
    for entry in &file.workspaces {
        let relative = [Some(&entry.path), Some(&entry.state)]
            .into_iter()
            .chain([entry.bindings.as_ref(), entry.tasks.as_ref()])
            .flatten()
            .any(|path| !path.is_absolute());
        if relative {
            return Err(format!("Workspace {} names a relative path", entry.id));
        }
    }
    Ok(file.workspaces)
}

/// Replace the file atomically with `workspaces`, sorted by label.
pub(super) fn write(location: &Path, mut workspaces: Vec<WorkspacePaths>) -> Result<(), String> {
    workspaces.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
    let parent = location
        .parent()
        .ok_or("the Workspaces file has no directory")?;
    let text = toml::to_string(&WorkspacesFile {
        version: VERSION,
        workspaces,
    })
    .map_err(|error| error.to_string())?;
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    staged
        .write_all(
            format!(
                "# This machine's paths of the Project's Workspaces; `af project workspaces` \
                 reads this (ADR-0158).\n{text}"
            )
            .as_bytes(),
        )
        .map_err(|error| error.to_string())?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    staged
        .persist(location)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}
