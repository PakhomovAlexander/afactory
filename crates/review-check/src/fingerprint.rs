//! The check fingerprint (telemetry design §9, `af/check-fingerprint@1`): the comparability key
//! of one check execution. Two executions with one fingerprint ran the same declared check, by
//! the same executor, under the same effective environment, on the same Snapshot; with both
//! `passed` and `failed` among their outcomes they are a flake.
//!
//! The fingerprint is `sha256:` and the 64 hex digits of SHA-256 over the canonical JSON of a
//! [`CheckFingerprintV1`]: the declared check (program, resolved arguments, timeout,
//! working-directory policy), the executor identity (platform triple, the resolved program's
//! digest, the toolchain snapshot id when one is declared), a digest of the effective
//! environment and the Snapshot digest. The environment is every name and value the check ran
//! with, after the runtime paths the runner introduced — the sandbox root, `CARGO_HOME`,
//! `RUSTUP_HOME`, `CARGO_TARGET_DIR`, the warm cache directories, the temporary directory — are
//! replaced by placeholders; it is hashed and never retained. When a part is unavailable, or
//! the caller did not name the runtime paths it introduced, the fingerprint is `unknown`.
//!
//! It is Telemetry's: recorded on the `af.check` span only, computed only when that span is
//! recorded, and read by nothing the kernel decides with (ADR-0153).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The versioned type of the fingerprinted document.
pub const CHECK_FINGERPRINT_V1: &str = "af/check-fingerprint@1";

/// The value of `af.check.fingerprint` when a part is unavailable.
pub const UNKNOWN: &str = "unknown";

/// The working-directory policy of a host-local check: the root of the materialized Snapshot.
pub const SANDBOX_ROOT: &str = "sandbox_root";

/// The placeholder the sandbox root becomes in the effective environment.
pub const SANDBOX_PLACEHOLDER: &str = "<sandbox>";

/// What a caller that introduced runtime paths declares, so the fingerprint can be computed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FingerprintInputs {
    /// The Snapshot the check ran on.
    pub snapshot_id: String,
    /// `None` when no toolchain is declared; `Some(None)` when one is declared and its snapshot
    /// id is unavailable (the fingerprint is then `unknown`).
    pub toolchain: Option<Option<String>>,
    /// Every runtime path the caller introduced into the environment, each with the name of the
    /// placeholder it becomes (`<runtime>`, `<cargo_home>`, `<warm:cargo_target>`, …). The
    /// sandbox root is the runner's own and always replaced.
    pub runtime_paths: Vec<(String, PathBuf)>,
}

/// The declared check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredCheckV1 {
    pub program: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
    pub working_directory: String,
}

/// Who executed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorV1 {
    /// The platform triple, such as `aarch64-apple-darwin`.
    pub platform: String,
    /// The digest of the resolved program's bytes.
    pub program_digest: String,
    /// The toolchain snapshot id, when one is declared.
    pub toolchain: Option<String>,
}

/// `af/check-fingerprint@1`: the document whose canonical JSON the fingerprint hashes. It holds
/// the environment only as a digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckFingerprintV1 {
    pub schema: String,
    pub check: DeclaredCheckV1,
    pub executor: ExecutorV1,
    /// `sha256:` over the canonical JSON of the effective environment, placeholders in place.
    pub environment_digest: String,
    pub snapshot_id: String,
}

impl CheckFingerprintV1 {
    /// The fingerprint: SHA-256 over the canonical JSON of this document.
    pub fn fingerprint(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|error| error.to_string())?;
        let bytes =
            review_store::canonical::canonicalize(&value).map_err(|error| error.to_string())?;
        Ok(sha256(&bytes))
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        review_core::hex::encode(Sha256::digest(bytes).as_slice())
    )
}

/// The platform triple of this build.
pub fn platform_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "macos" => format!("{arch}-apple-darwin"),
        "linux" if cfg!(target_env = "musl") => format!("{arch}-unknown-linux-musl"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        os => format!("{arch}-unknown-{os}"),
    }
}

/// The environment a check ran with, as `Command::env` applies it (a later value of a name
/// replaces an earlier one), with every runtime path replaced by its placeholder, longest path
/// first so a nested one is never half-replaced; its digest. The plaintext never leaves here.
pub fn environment_digest(
    environment: &[(String, String)],
    sandbox_root: &Path,
    runtime_paths: &[(String, PathBuf)],
) -> Result<String, String> {
    let mut replacements: Vec<(String, String)> = runtime_paths
        .iter()
        .map(|(placeholder, path)| (path.display().to_string(), format!("<{placeholder}>")))
        .chain([(
            sandbox_root.display().to_string(),
            SANDBOX_PLACEHOLDER.to_string(),
        )])
        .filter(|(path, _)| !path.is_empty() && path != "/")
        .collect();
    replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    let effective: BTreeMap<&str, String> = environment
        .iter()
        .map(|(name, value)| {
            let value = replacements
                .iter()
                .fold(value.clone(), |value, (path, placeholder)| {
                    value.replace(path.as_str(), placeholder)
                });
            (name.as_str(), value)
        })
        .collect();
    let value = serde_json::to_value(&effective).map_err(|error| error.to_string())?;
    Ok(sha256(
        &review_store::canonical::canonicalize(&value).map_err(|error| error.to_string())?,
    ))
}

/// The program a check names, resolved as the process start resolves it: a name with a `/`
/// against the working directory, a bare name through the environment's `PATH`.
pub fn resolve_program(
    program: &str,
    workdir: &Path,
    environment: &[(String, String)],
) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let path = workdir.join(program);
        return path.is_file().then_some(path);
    }
    let path = environment
        .iter()
        .rev()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value.clone())?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// The digest of a resolved program's bytes, remembered per file identity (device, inode, size
/// and modification time) for the life of the process. `None` when it cannot be read.
pub fn program_digest(path: &Path) -> Option<String> {
    type Identity = (PathBuf, u64, u64, u64, std::time::SystemTime);
    static DIGESTS: Mutex<Option<HashMap<Identity, String>>> = Mutex::new(None);
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    let identity = (
        path.to_path_buf(),
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.modified().ok()?,
    );
    let mut digests = DIGESTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(digest) = digests.as_ref().and_then(|map| map.get(&identity)) {
        return Some(digest.clone());
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        use std::io::Read as _;
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    let digest = format!(
        "sha256:{}",
        review_core::hex::encode(hasher.finalize().as_slice())
    );
    digests
        .get_or_insert_with(HashMap::new)
        .insert(identity, digest.clone());
    Some(digest)
}

/// Everything one local check execution's fingerprint covers.
pub struct Execution<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub timeout: std::time::Duration,
    pub workdir: &'a Path,
    pub environment: &'a [(String, String)],
}

/// The fingerprint of one host-local execution, or [`UNKNOWN`] when a part is unavailable:
/// no declared inputs (the runner cannot name the runtime paths the caller introduced), a
/// program that does not resolve or cannot be read, or a declared toolchain without its id.
pub fn fingerprint(execution: &Execution<'_>, inputs: Option<&FingerprintInputs>) -> String {
    compute(execution, inputs).unwrap_or_else(|| UNKNOWN.to_string())
}

fn compute(execution: &Execution<'_>, inputs: Option<&FingerprintInputs>) -> Option<String> {
    let inputs = inputs?;
    if !review_core::is_digest(&inputs.snapshot_id) {
        return None;
    }
    let toolchain = match &inputs.toolchain {
        None => None,
        Some(Some(id)) => Some(id.clone()),
        Some(None) => return None,
    };
    let program = resolve_program(execution.program, execution.workdir, execution.environment)?;
    let document = CheckFingerprintV1 {
        schema: CHECK_FINGERPRINT_V1.into(),
        check: DeclaredCheckV1 {
            program: execution.program.to_string(),
            args: execution.args.to_vec(),
            timeout_ms: u64::try_from(execution.timeout.as_millis()).unwrap_or(u64::MAX),
            working_directory: SANDBOX_ROOT.into(),
        },
        executor: ExecutorV1 {
            platform: platform_triple(),
            program_digest: program_digest(&program)?,
            toolchain,
        },
        environment_digest: environment_digest(
            execution.environment,
            execution.workdir,
            &inputs.runtime_paths,
        )
        .ok()?,
        snapshot_id: inputs.snapshot_id.clone(),
    };
    document.fingerprint().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_path_becomes_its_placeholder_and_the_longest_path_wins() {
        let environment = |root: &str| {
            vec![
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("HOME".to_string(), format!("{root}/runtime/home")),
                (
                    "CARGO_TARGET_DIR".to_string(),
                    format!("{root}/warm/target"),
                ),
                ("PWD".to_string(), format!("{root}/sandbox/tree")),
            ]
        };
        let paths = |root: &str| {
            vec![
                (
                    "runtime".to_string(),
                    PathBuf::from(format!("{root}/runtime")),
                ),
                ("warm".to_string(), PathBuf::from(format!("{root}/warm"))),
                (
                    "warm:cargo_target".to_string(),
                    PathBuf::from(format!("{root}/warm/target")),
                ),
            ]
        };
        let first = environment_digest(
            &environment("/tmp/a"),
            Path::new("/tmp/a/sandbox/tree"),
            &paths("/tmp/a"),
        )
        .unwrap();
        let second = environment_digest(
            &environment("/var/b"),
            Path::new("/var/b/sandbox/tree"),
            &paths("/var/b"),
        )
        .unwrap();
        assert_eq!(first, second, "runtime paths are placeholders");
        let mut changed = environment("/tmp/a");
        changed[0].1 = "/opt/bin:/usr/bin:/bin".into();
        assert_ne!(
            first,
            environment_digest(&changed, Path::new("/tmp/a/sandbox/tree"), &paths("/tmp/a"))
                .unwrap()
        );
        // A later value of one name replaces the earlier one, as the process sees it.
        let mut repeated = environment("/tmp/a");
        repeated.push(("PATH".into(), "/usr/bin:/bin".into()));
        assert_eq!(
            first,
            environment_digest(
                &repeated,
                Path::new("/tmp/a/sandbox/tree"),
                &paths("/tmp/a")
            )
            .unwrap()
        );
    }
}
