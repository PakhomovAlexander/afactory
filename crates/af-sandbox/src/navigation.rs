//! Credential-free, source-only navigation. No Provider, Task scheduling or spending authority.
use std::{
    collections::BTreeMap,
    io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub const IMAGE: &str =
    "python@sha256:aa943ce411423c7582288f18707ff66eddcbaeb510502d747a72aeb34f30d159";
pub const IDENTITY: &str = "af.source-navigation/3";
const WORKER: &str = include_str!("navigation.py");
const MAX_RESPONSE: usize = 262_144;
const MAX_FILES: usize = 16_384;
const MAX_SOURCE_BYTES: usize = 128 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const PAGE_SIZE: usize = 256;

pub mod semantic;

use af_contracts::navigation::valid_path;
pub use af_contracts::navigation::{Hit, Request, Response};
fn denied() -> io::Error {
    io::Error::other("source navigation refused or backend unavailable")
}

/// Host-owned exact source staging. Inputs are admitted file bytes, never a checkout path.
/// All links/special files are unrepresentable. The host must select source without oracle data.
/// No staging path or mutable handle is exposed. Only Docker is supported; no local fallback.
pub struct Broker {
    staging: Option<tempfile::TempDir>,
    source_identity: String,
    has_analyzer_config: bool,
}
impl Broker {
    pub fn source_identity(&self) -> &str {
        &self.source_identity
    }
    /// Pins protocol, schema, host/worker bytes and image. Not a conformance pass receipt.
    pub fn conformance_identity() -> String {
        af_source_git::digest_bytes(
            &serde_json::to_vec(&(
                IDENTITY,
                IMAGE,
                WORKER,
                include_str!("navigation.rs"),
                include_str!("../../af-contracts/src/navigation.rs"),
                include_str!("../../../schemas/source-navigation-v3.json"),
            ))
            .expect("static strings serialize"),
        )
    }
    pub fn request_identity(request: &Request) -> String {
        af_source_git::digest_bytes(&serde_json::to_vec(request).expect("typed request serializes"))
    }
    pub fn response_identity(response: &Response) -> String {
        af_source_git::digest_bytes(
            &serde_json::to_vec(response).expect("typed response serializes"),
        )
    }

    /// Retains all native Manifest entries, including binary bytes and executable provenance.
    /// Execution is still forbidden: staged files are uniformly read-only and non-executable.
    pub fn from_manifest(
        manifest: &af_source_git::Manifest,
        cas: &af_store::Cas,
    ) -> io::Result<Self> {
        manifest.validate().map_err(|_| denied())?;
        if manifest.entries.is_empty() || manifest.entries.len() > MAX_FILES {
            return Err(denied());
        }
        let mut files = BTreeMap::new();
        let mut total = 0u64;
        for entry in &manifest.entries {
            total = total.checked_add(entry.size).ok_or_else(denied)?;
            if !valid_path(&entry.path)
                || entry.kind == af_source_git::EntryKind::Symlink
                || entry.size > MAX_FILE_BYTES as u64
                || total > MAX_SOURCE_BYTES as u64
            {
                return Err(denied());
            }
            let bytes = cas
                .get_bounded(&entry.content, MAX_FILE_BYTES as u64)
                .map_err(|_| denied())?;
            if bytes.len() as u64 != entry.size
                || af_source_git::digest_bytes(&bytes) != entry.content
            {
                return Err(denied());
            }
            files.insert(entry.path.clone(), bytes);
        }
        let mut broker = Self::new(files)?;
        broker.source_identity = manifest.content_digest();
        Ok(broker)
    }

    pub fn new(files: BTreeMap<String, Vec<u8>>) -> io::Result<Self> {
        Self::stage_in(files, &std::env::temp_dir())
    }
    fn stage_in(files: BTreeMap<String, Vec<u8>>, temporary_root: &Path) -> io::Result<Self> {
        if files.is_empty() || files.len() > MAX_FILES {
            return Err(denied());
        }
        let mut total = 0usize;
        let mut spellings = BTreeMap::new();
        for (path, bytes) in &files {
            if !valid_path(path) {
                return Err(denied());
            }
            // Refuse aliases deterministically, including on case-insensitive macOS volumes.
            let mut prefix = String::new();
            for component in path.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                if spellings
                    .insert(prefix.to_ascii_lowercase(), prefix.clone())
                    .is_some_and(|previous| previous != prefix)
                {
                    return Err(denied());
                }
            }
            total = total.checked_add(bytes.len()).ok_or_else(denied)?;
            if total > MAX_SOURCE_BYTES || bytes.len() > MAX_FILE_BYTES {
                return Err(denied());
            }
        }
        let source_identity = af_source_git::Manifest::new(
            files
                .iter()
                .map(|(path, bytes)| af_source_git::Entry {
                    path: path.clone(),
                    kind: af_source_git::EntryKind::File,
                    content: af_source_git::digest_bytes(bytes),
                    size: bytes.len() as u64,
                })
                .collect(),
        )
        .map_err(|_| denied())?
        .content_digest();
        let has_analyzer_config = files.keys().any(|path| {
            path.rsplit('/')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("rust-analyzer.toml"))
        });
        let staging = tempfile::Builder::new()
            .prefix("af-navigation-")
            .tempdir_in(temporary_root)?;
        for (path, bytes) in files {
            let file = staging.path().join("source").join(path);
            std::fs::create_dir_all(file.parent().ok_or_else(denied)?)?;
            std::fs::write(&file, bytes)?;
            mode(&file, 0o444)?;
        }
        fn directories(path: &Path) -> io::Result<()> {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    directories(&entry.path())?;
                }
            }
            mode(path, 0o555)
        }
        // The private envelope stays 0700, including when staging is preserved. Docker
        // mounts only its child, so the mapped nonroot user need not traverse the envelope.
        mode(staging.path(), 0o700)?;
        directories(&staging.path().join("source"))?;
        Ok(Self {
            staging: Some(staging),
            source_identity,
            has_analyzer_config,
        })
    }
    /// One bounded invocation, followed by confirmed Docker removal (also on timeout).
    /// Cleanup failure preserves staging and returns an error; it never yields a response.
    pub fn execute(&mut self, request: &Request) -> io::Result<Response> {
        self.execute_program(request, WORKER, Duration::from_secs(30))
    }
    /// Shares the subject's absolute deadline across container creation and navigation.
    /// After expiry only exact-container cleanup may run, with its own bounded grace.
    /// An already expired request cannot create a container or launch a Docker helper.
    pub fn execute_until(&mut self, request: &Request, deadline: Instant) -> io::Result<Response> {
        self.execute_program_until(request, WORKER, deadline)
    }
    fn execute_program(
        &mut self,
        request: &Request,
        program: &str,
        timeout: Duration,
    ) -> io::Result<Response> {
        self.execute_program_until(request, program, Instant::now() + timeout)
    }
    fn execute_program_until(
        &mut self,
        request: &Request,
        program: &str,
        deadline: Instant,
    ) -> io::Result<Response> {
        request.validate()?;
        self.run_container(
            program,
            "128m",
            &[],
            serde_json::to_vec(request).map_err(io::Error::other)?,
            deadline,
        )
        .and_then(|bytes| decode(request, &bytes))
    }

    fn run_container(
        &mut self,
        program: &str,
        memory: &str,
        extra: &[&str],
        input: Vec<u8>,
        deadline: Instant,
    ) -> io::Result<Vec<u8>> {
        remaining(deadline)?;
        let name = self
            .staging
            .as_ref()
            .ok_or_else(denied)?
            .path()
            .file_name()
            .ok_or_else(denied)?
            .to_str()
            .ok_or_else(denied)?
            .to_owned();
        let mount = read_only_mount(
            &self
                .staging
                .as_ref()
                .ok_or_else(denied)?
                .path()
                .join("source"),
            "/source",
        )?;
        // Only a successfully created, exact container ID grants start/removal authority.
        // A timed-out create may still finish at the daemon: preserve its mount and stop.
        let id =
            match create_with(&name, &mount, program, memory, extra, deadline).and_then(|bytes| {
                let id = std::str::from_utf8(&bytes).map_err(|_| denied())?.trim();
                if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(denied());
                }
                Ok(id.to_owned())
            }) {
                Ok(id) => id,
                Err(_) => return Err(self.preserve()),
            };
        let result = docker_until(&["start", "--attach", "-i", &id], Some(input), deadline);
        // rm -f ends the container's entire PID namespace, unlike killing the Docker client.
        if docker(&["rm", "--force", &id], None).is_err() {
            return Err(self.preserve());
        }
        // Cleanup can consume its bounded grace after expiry, but no source response can
        // extend the subject's allowance. The host retains the failed navigation attempt.
        remaining(deadline)?;
        result
    }
    fn preserve(&mut self) -> io::Error {
        match self.staging.take() {
            Some(staging) => {
                let path = staging.keep();
                io::Error::other(format!(
                    "navigation create/cleanup unconfirmed; broker disabled; staging preserved at {}",
                    path.display()
                ))
            }
            None => denied(),
        }
    }
}
fn read_only_mount(source: &Path, destination: &str) -> io::Result<String> {
    let source = source.to_str().ok_or_else(denied)?;
    if source.contains([',', '"', '\n', '\r']) {
        return Err(denied());
    }
    Ok(format!("type=bind,src={source},dst={destination},readonly"))
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .map(|remaining| remaining.min(Duration::from_secs(30)))
        .ok_or_else(denied)
}
fn decode(request: &Request, bytes: &[u8]) -> io::Result<Response> {
    if bytes.len() > MAX_RESPONSE {
        return Err(denied());
    }
    let response: Response = serde_json::from_slice(bytes).map_err(|_| denied())?;
    if std::mem::discriminant(request)
        != match &response {
            Response::ListPaths { .. } => std::mem::discriminant(&Request::ListPaths { offset: 0 }),
            Response::ReadRange { .. } => std::mem::discriminant(&Request::ReadRange {
                path: String::new(),
                offset: 0,
                length: 1,
            }),
            Response::Search { .. } => std::mem::discriminant(&Request::Search {
                query: String::new(),
                offset: 0,
            }),
        }
    {
        return Err(denied());
    }
    let valid = match (&response, request) {
        (Response::ListPaths { paths, next_offset }, Request::ListPaths { offset }) => {
            paths.len() <= PAGE_SIZE
                && next_offset.is_none_or(|next| {
                    paths.len() == PAGE_SIZE
                        && next == *offset + PAGE_SIZE as u32
                        && (next as usize) < MAX_FILES
                })
                && paths.iter().all(|p| valid_path(p))
                && paths.windows(2).all(|w| w[0] < w[1])
        }
        (Response::ReadRange { text }, Request::ReadRange { length, .. }) => text
            .as_ref()
            .is_none_or(|text| text.len() <= *length as usize),
        (
            Response::Search {
                hits,
                next_offset,
                non_text_paths,
            },
            Request::Search { query, offset },
        ) => {
            hits.len() <= 128
                && next_offset
                    .is_none_or(|next| hits.len() == 128 && offset.checked_add(128) == Some(next))
                && non_text_paths.len() <= MAX_FILES
                && non_text_paths.iter().all(|path| valid_path(path))
                && non_text_paths.windows(2).all(|paths| paths[0] < paths[1])
                && hits
                    .iter()
                    .all(|h| valid_path(&h.path) && h.line > 0 && h.text.contains(query))
                && hits
                    .windows(2)
                    .all(|w| (&w[0].path, w[0].line) < (&w[1].path, w[1].line))
        }
        _ => false,
    };
    if !valid {
        return Err(denied());
    }
    Ok(response)
}
fn docker(args: &[&str], input: Option<Vec<u8>>) -> io::Result<Vec<u8>> {
    docker_with_timeout(args, input, Duration::from_secs(30))
}
fn docker_with_timeout(
    args: &[&str],
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    docker_until(args, input, Instant::now() + timeout)
}
fn docker_until(args: &[&str], input: Option<Vec<u8>>, deadline: Instant) -> io::Result<Vec<u8>> {
    remaining(deadline)?;
    let client = tempfile::tempdir()?;
    let mut command = Command::new("docker");
    command.arg("--config").arg(client.path());
    command
        .env_clear()
        .env("PATH", af_process::sanitized_path())
        .args(args);
    let output = af_process::run_supervised_with_policy(
        &mut command,
        input,
        remaining(deadline)?,
        af_process::ExitPolicy::KillProcessGroup,
    )
    .map_err(|_| denied())?;
    if !output.status.success()
        || output.stderr_held
        || output.truncation.is_truncated()
        || output.stdout.len() > MAX_RESPONSE
    {
        return Err(denied());
    }
    Ok(output.stdout)
}
#[cfg(unix)]
fn mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}
#[cfg(not(unix))]
fn mode(_: &Path, _: u32) -> io::Result<()> {
    Err(denied())
}

impl Drop for Broker {
    fn drop(&mut self) {
        fn unlock(path: &Path) {
            let _ = mode(path, 0o700);
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        unlock(&entry.path());
                    }
                }
            }
        }
        if let Some(staging) = &self.staging {
            unlock(staging.path());
        }
    }
}
#[cfg(test)]
mod tests;

#[cfg(test)]
fn create(name: &str, mount: &str, program: &str) -> io::Result<Vec<u8>> {
    create_with(
        name,
        mount,
        program,
        "128m",
        &[],
        Instant::now() + Duration::from_secs(30),
    )
}
fn create_with(
    name: &str,
    mount: &str,
    program: &str,
    memory: &str,
    extra: &[&str],
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    let mut args = vec![
        "create",
        "--name",
        name,
        "--pull",
        "never",
        "--network",
        "none",
        "--read-only",
        "--user",
        "65534:65534",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "32",
        "--memory",
        memory,
        "--cpus",
        "1",
        "--mount",
        mount,
        "--workdir",
        "/source",
        "-i",
    ];
    args.extend_from_slice(extra);
    args.extend_from_slice(&[IMAGE, "python3", "-I", "-B", "-c", program]);
    docker_until(&args, None, deadline)
}
