use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use af_contracts::fleet::Freshness;
use af_process::{ExitPolicy, run_supervised_captured_bounded_until};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub(super) const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;
pub(super) const POOLS: &str = "control-api http://localhost/pools";
pub(super) const ALLOCATIONS: &str = "control-api http://localhost/dynamic/allocations";
pub(super) const ORIGIN: &str = "af cluster origin-read";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Target {
    host: String,
    port: u16,
    user: String,
    host_key: String,
}

impl Target {
    pub(super) fn validate(&self) -> bool {
        let Some((kind, key)) = self.host_key.split_once(' ') else {
            return false;
        };
        matches!(
            kind,
            "ssh-ed25519"
                | "ssh-rsa"
                | "ecdsa-sha2-nistp256"
                | "ecdsa-sha2-nistp384"
                | "ecdsa-sha2-nistp521"
        ) && (32..=4096).contains(&key.len())
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
            && self.port != 0
            && self.host.len() <= 253
            && self
                .host
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
            && super::is_label(&self.user)
    }

    pub(super) fn read<T: DeserializeOwned>(
        &self,
        key: &Path,
        operation: &'static str,
        deadline: Instant,
    ) -> Result<T, ReadFailure> {
        // OpenSSH also expands these tokens in its IdentityFile paths.
        if !key.is_file()
            || key.to_str().is_none_or(|text| {
                text.chars()
                    .any(|ch| ch.is_control() || "%${}".contains(ch))
            })
        {
            return Err(ReadFailure::MissingCredential);
        }
        let mut known_hosts =
            tempfile::NamedTempFile::new().map_err(|_| ReadFailure::Unavailable)?;
        writeln!(known_hosts, "af-fleet-target {}", self.host_key)
            .map_err(|_| ReadFailure::Unavailable)?;
        let known_hosts_path = known_hosts
            .path()
            .to_str()
            .ok_or(ReadFailure::Unavailable)?;
        // OpenSSH tokenizes -o values and expands percent escapes and environment references.
        // Refuse unusual temporary paths rather than passing one through that expansion.
        if known_hosts_path
            .chars()
            .any(|ch| ch.is_control() || "%${}\\\"".contains(ch))
        {
            return Err(ReadFailure::Unavailable);
        }
        let port = self.port.to_string();
        let mut command = Command::new("ssh");
        command
            .env("PATH", af_process::sanitized_path())
            .env("LC_ALL", "C")
            .args([
                "-F",
                "/dev/null",
                "-n",
                "-T",
                "-p",
                &port,
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                "HostKeyAlias=af-fleet-target",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "CheckHostIP=no",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "ForwardAgent=no",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ControlMaster=no",
                "-o",
                "ConnectTimeout=5",
                "-o",
                &format!("UserKnownHostsFile=\"{known_hosts_path}\""),
                "-l",
                &self.user,
                "-i",
            ])
            .arg(key)
            .args(["--", &self.host, operation]);
        let output = run_supervised_captured_bounded_until(
            &mut command,
            None,
            deadline,
            ExitPolicy::KillProcessGroup,
            None,
            MAX_REPLY_BYTES,
        )
        .into_result()
        .map_err(|_| ReadFailure::Unavailable)?;
        if output.truncation.is_truncated() || output.stderr_held {
            return Err(ReadFailure::InvalidResponse);
        }
        match output.status.code() {
            Some(0) => {
                serde_json::from_slice(&output.stdout).map_err(|_| ReadFailure::InvalidResponse)
            }
            Some(75) => Err(ReadFailure::Temporary),
            Some(77) => Err(ReadFailure::Denied),
            Some(255) => {
                // OpenSSH uses the same exit code for handshake, access and transport failures.
                // Its diagnostic only refines the refusal. No error here can authorize a read.
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("Host key verification failed") {
                    Err(ReadFailure::IdentityMismatch)
                } else if stderr.contains("Permission denied (") {
                    Err(ReadFailure::Denied)
                } else {
                    Err(ReadFailure::Unavailable)
                }
            }
            _ => Err(ReadFailure::Unavailable),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ReadFailure {
    Denied,
    IdentityMismatch,
    MissingCredential,
    InvalidResponse,
    Temporary,
    Unavailable,
}

impl ReadFailure {
    pub(super) fn freshness(self) -> Freshness {
        match self {
            Self::Denied => Freshness::Denied,
            Self::IdentityMismatch => Freshness::IdentityMismatch,
            Self::MissingCredential => Freshness::MissingCredential,
            Self::InvalidResponse => Freshness::InvalidResponse,
            Self::Temporary | Self::Unavailable => Freshness::Unavailable,
        }
    }

    pub(super) fn reason(self) -> &'static str {
        match self {
            Self::Denied => "read access denied",
            Self::IdentityMismatch => "server identity mismatch",
            Self::MissingCredential => "local SSH credential reference could not be resolved",
            Self::InvalidResponse => "invalid observation response",
            Self::Temporary | Self::Unavailable => "reader unavailable",
        }
    }
}
