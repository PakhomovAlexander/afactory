//! Explicitly configured rust-analyzer in the source-only Docker broker.
use super::{Broker, denied, mode, remaining, valid_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, path::Path, time::Instant};

const DRIVER: &str = include_str!("semantic.py");
pub const VERSION: &str = "1.88.0";
pub const MAX_BINARY: u64 = 64 * 1024 * 1024;
pub const MAX_ANCHORS: usize = 64;
pub const LINUX_ANALYZER_SHA256: &str =
    "sha256:b64eb7065e3931f01fc9560a9409c241c43088d056d04368898b2d9ea04aa703";
pub const LINUX_LIBRARIES: [(&str, &str, u64); 2] = [
    (
        "librustc_driver-277627f6f3b9f29c.so",
        "sha256:ce408ed4018b56fe4175475b516b1440946bcaf0edf33f957ac581b405c8a9b4",
        144_531_472,
    ),
    (
        "libLLVM.so.20.1-rust-1.88.0-stable",
        "sha256:e03afb05eeb8c5f3354ff7ea5a839bae7f7b79ec1a5096f4611072e7313ef72f",
        186_158_864,
    ),
];

pub struct LinuxRuntime<'a> {
    pub analyzer: &'a [u8],
    pub libraries: [&'a [u8]; 2],
}

fn matches_pin(bytes: &[u8], pin: &str) -> bool {
    let digest = af_contracts::hex::encode(&Sha256::digest(bytes));
    pin.strip_prefix("sha256:") == Some(&digest)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Definition,
    References,
    Implementation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub path: String,
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub path: String,
    pub line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Crate {
    pub root: String,
    pub edition: Edition,
    pub dependencies: Vec<Dependency>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edition {
    #[serde(rename = "2018")]
    Rust2018,
    #[serde(rename = "2021")]
    Rust2021,
    #[serde(rename = "2024")]
    Rust2024,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub name: String,
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub anchors: Vec<Anchor>,
    pub truncated: bool,
}

pub fn config_identity() -> String {
    af_source_git::digest_bytes(
        &[VERSION, super::IMAGE, DRIVER, include_str!("semantic.rs")]
            .concat()
            .into_bytes(),
    )
}

pub fn validate_project(project: &[Crate], has_path: impl Fn(&str) -> bool) -> io::Result<()> {
    if project.is_empty() || project.len() > 64 {
        return Err(denied());
    }
    for (index, krate) in project.iter().enumerate() {
        let mut names = std::collections::BTreeSet::new();
        if !valid_path(&krate.root)
            || !krate.root.ends_with(".rs")
            || !has_path(&krate.root)
            || krate.dependencies.len() > 64
            || krate.dependencies.iter().any(|dep| {
                dep.index >= index
                    || !names.insert(&dep.name)
                    || dep.name.is_empty()
                    || dep.name.len() > 64
                    || !dep
                        .name
                        .starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
                    || !dep
                        .name
                        .bytes()
                        .all(|b| b == b'_' || b.is_ascii_alphanumeric())
            })
        {
            return Err(denied());
        }
    }
    Ok(())
}

impl Broker {
    pub fn semantic(
        &mut self,
        binary: &[u8],
        binary_digest: &str,
        project: &[Crate],
        operation: Operation,
        position: &Position,
        deadline: Instant,
    ) -> io::Result<Reply> {
        if binary.is_empty()
            || binary.len() as u64 > MAX_BINARY
            || af_source_git::digest_bytes(binary) != binary_digest
        {
            return Err(denied());
        }
        self.semantic_query(binary, &[], project, operation, position, deadline)
    }

    pub fn semantic_linux(
        &mut self,
        runtime: &LinuxRuntime<'_>,
        project: &[Crate],
        operation: Operation,
        position: &Position,
        deadline: Instant,
    ) -> io::Result<Reply> {
        remaining(deadline)?;
        if runtime.analyzer.len() as u64 > MAX_BINARY
            || !matches_pin(runtime.analyzer, LINUX_ANALYZER_SHA256)
        {
            return Err(denied());
        }
        let mut libraries = Vec::new();
        for ((name, digest, size), bytes) in LINUX_LIBRARIES.into_iter().zip(runtime.libraries) {
            if bytes.len() as u64 != size || !matches_pin(bytes, digest) {
                return Err(denied());
            }
            libraries.push((name, bytes));
        }
        self.semantic_query(
            runtime.analyzer,
            &libraries,
            project,
            operation,
            position,
            deadline,
        )
    }

    fn semantic_query(
        &mut self,
        binary: &[u8],
        libraries: &[(&str, &[u8])],
        project: &[Crate],
        operation: Operation,
        position: &Position,
        deadline: Instant,
    ) -> io::Result<Reply> {
        remaining(deadline)?;
        if self.has_analyzer_config {
            return Err(denied());
        }
        let staging = self.staging.as_ref().ok_or_else(denied)?.path().to_owned();
        let source = staging.join("source");
        let has_path = |path: &str| valid_path(path) && source.join(path).is_file();
        validate_project(project, has_path)?;
        if !has_path(&position.path) {
            return Err(denied());
        }
        let runtime = tempfile::tempdir_in(&staging)?;
        mode(runtime.path(), 0o755)?;
        stage_file(&runtime.path().join("rust-analyzer"), binary, 0o555)?;
        for (name, bytes) in libraries {
            stage_file(&runtime.path().join(name), bytes, 0o444)?;
        }
        let mount = super::read_only_mount(runtime.path(), "/analyzer")?;
        let input = serde_json::to_vec(&serde_json::json!({
            "project": project, "operation": operation, "position": position,
            "version": VERSION, "limit": MAX_ANCHORS
        }))
        .map_err(io::Error::other)?;
        let result = self.run_container(DRIVER, "768m", &["--mount", &mount], input, deadline);
        if self.staging.is_none() {
            // An unconfirmed container may still need its exact original mounts.
            let _ = runtime.keep();
        }
        let bytes = result?;
        let reply: Reply = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if reply.anchors.len() > MAX_ANCHORS
            || !reply.anchors.windows(2).all(|pair| pair[0] < pair[1])
            || reply
                .anchors
                .iter()
                .any(|a| a.line == 0 || !has_path(&a.path))
        {
            return Err(denied());
        }
        Ok(reply)
    }
}

fn stage_file(path: &Path, bytes: &[u8], permissions: u32) -> io::Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    mode(path, permissions)
}
