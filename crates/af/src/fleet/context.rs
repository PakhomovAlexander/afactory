use std::collections::BTreeMap;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::is_label;
use super::transport::Target;

const DESCRIPTOR_SCHEMA: &str = "af/fleet-context@1";
const BINDING_SCHEMA: &str = "af/fleet-binding@1";
const MAX_CONTEXT_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Origin {
    pub target: Target,
    pub project_id: String,
    pub store_id: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Descriptor {
    schema: String,
    pub scope: String,
    pub management: Target,
    #[serde(default)]
    pub cluster: af_contracts::fleet::ClusterState,
    #[serde(default)]
    pub origins: BTreeMap<String, Origin>,
}

impl Descriptor {
    pub(super) fn load(path: &Path) -> Result<Self, String> {
        let descriptor: Self = load(path)?.ok_or("Connection descriptor is missing")?;
        descriptor.validate()?;
        Ok(descriptor)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != DESCRIPTOR_SCHEMA
            || !is_label(&self.scope)
            || !self.management.validate()
            || self.origins.len() > 16
            || self.origins.iter().any(|(name, origin)| {
                !is_label(name)
                    || !af_contracts::is_digest(&origin.project_id)
                    || !af_contracts::is_digest(&origin.store_id)
                    || !origin.target.validate()
            })
        {
            return Err("Invalid fleet connection descriptor or version".into());
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    schema: String,
    pub credential_ref: String,
    pub descriptor: Descriptor,
}

impl Binding {
    pub(super) fn new(descriptor: Descriptor, credential_ref: String) -> Self {
        Self {
            schema: BINDING_SCHEMA.into(),
            credential_ref,
            descriptor,
        }
    }
}

pub(super) struct Config {
    root: PathBuf,
}

impl Config {
    pub(super) fn open() -> Result<Self, String> {
        Ok(Self {
            root: crate::config::config_home()?.join("af/fleet"),
        })
    }

    fn bindings(&self) -> PathBuf {
        self.root.join("contexts")
    }

    fn binding_path(&self, alias: &str) -> PathBuf {
        self.bindings().join(format!("{alias}.json"))
    }

    pub(super) fn aliases(&self) -> Result<Vec<String>, String> {
        let entries = match std::fs::read_dir(self.bindings()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("Listing fleet contexts: {error}")),
        };
        let mut aliases = Vec::new();
        for entry in entries {
            let name = entry.map_err(|error| error.to_string())?.file_name();
            if let Some(alias) = name.to_str().and_then(|name| name.strip_suffix(".json"))
                && is_label(alias)
            {
                aliases.push(alias.to_owned());
            }
        }
        aliases.sort_unstable();
        Ok(aliases)
    }

    pub(super) fn binding(&self, alias: &str) -> Result<Option<Binding>, String> {
        let binding: Option<Binding> = load(&self.binding_path(alias))?;
        if let Some(binding) = &binding {
            if binding.schema != BINDING_SCHEMA || !is_label(&binding.credential_ref) {
                return Err("Invalid fleet binding or version".into());
            }
            binding.descriptor.validate()?;
        }
        Ok(binding)
    }

    pub(super) fn publish(&self, alias: &str, binding: &Binding) -> Result<(), String> {
        let path = self.binding_path(alias);
        af_store::local_fs::create_private_dir_all(&self.bindings())
            .map_err(|error| format!("Creating fleet configuration: {error}"))?;
        let temporary = write_record(&self.bindings(), binding)?;
        match temporary.persist_noclobber(path) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
                if self.binding(alias)?.as_ref() == Some(binding) {
                    Ok(())
                } else {
                    Err(conflict(alias))
                }
            }
            Err(error) => Err(format!("Saving fleet binding: {}", error.error)),
        }
    }

    pub(super) fn management_key(&self, reference: &str) -> PathBuf {
        self.root.join("credentials").join(reference)
    }

    pub(super) fn origin_key(&self, alias: &str, origin: &str) -> PathBuf {
        self.root
            .join("project-credentials")
            .join(alias)
            .join(origin)
    }
}

pub(super) fn conflict(alias: &str) -> String {
    format!("Context {alias} already has a different binding. Choose another context name")
}

fn load<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match af_store::local_fs::read_bounded(path, MAX_CONTEXT_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("Invalid fleet configuration at {}: {e}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "Reading fleet configuration at {}: {error}",
            path.display()
        )),
    }
}

pub(super) fn write_record<T: Serialize>(
    directory: &Path,
    value: &T,
) -> Result<tempfile::NamedTempFile, String> {
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, value).map_err(|error| error.to_string())?;
    file.write_all(b"\n")
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    Ok(file)
}
