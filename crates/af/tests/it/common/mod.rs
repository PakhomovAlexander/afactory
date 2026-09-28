//! A sandboxed self-managed layout for tests: XDG directories in a temporary root, a directory
//! release source, fake releases whose `af` is a shell script, optional minisign signatures made
//! the way the release job makes them, and the real binary adopted as a receipted install.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub const AF: &str = env!("CARGO_BIN_EXE_af");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TARGET: &str = env!("AF_TARGET");

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    review_core::hex::encode(&<sha2::Sha256 as sha2::Digest>::digest(bytes))
}

/// A minisign key pair for signing test releases; `AF_RELEASE_KEY` points the binary at it.
pub struct Signer {
    pair: minisign::KeyPair,
    pub public_key_file: PathBuf,
}

impl Signer {
    pub fn new(dir: &Path) -> Self {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let public_key_file = dir.join("release.pub");
        write(&public_key_file, &pair.pk.to_box().unwrap().into_string());
        Self {
            pair,
            public_key_file,
        }
    }

    pub fn sign(&self, data: &[u8]) -> String {
        minisign::sign(
            Some(&self.pair.pk),
            &self.pair.sk,
            data,
            Some("af test release"),
            None,
        )
        .unwrap()
        .into_string()
    }
}

pub struct Sandbox {
    root: tempfile::TempDir,
    key: Option<PathBuf>,
}

impl Sandbox {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for dir in [
            "home", "config", "data", "state", "cache", "bin", "releases",
        ] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        Self { root, key: None }
    }

    /// Every command from now on verifies `SHA256SUMS` signatures with this key.
    pub fn with_key(mut self, signer: &Signer) -> Self {
        self.key = Some(signer.public_key_file.clone());
        self
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    pub fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_BIN_HOME", self.path("bin"))
            .env("AF_RELEASE_SOURCE", self.path("releases"))
            .env("NO_COLOR", "1")
            .env_remove("AF_SELF_OFFLINE")
            .env_remove("AF_VERSION")
            .env_remove("AF_RELEASE_KEY")
            .env_remove("CI");
        if let Some(key) = &self.key {
            command.env("AF_RELEASE_KEY", key);
        }
        command
    }

    pub fn versions(&self) -> PathBuf {
        self.path("data/af/versions")
    }

    pub fn default_target(&self) -> Option<String> {
        std::fs::read_link(self.path("bin/af"))
            .ok()
            .and_then(|link| {
                link.parent()
                    .and_then(|dir| dir.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
            })
    }

    pub fn release_dir(&self, version: &str) -> PathBuf {
        self.path(&format!("releases/v{version}"))
    }

    pub fn asset(version: &str) -> String {
        format!("af-v{version}-{TARGET}.tar.gz")
    }

    /// A release whose `af` is a script reporting `version`, with an unsigned `SHA256SUMS`
    /// (tampered when asked). Returns the archive's real digest.
    pub fn publish(&self, version: &str, tamper: bool) -> String {
        let tag_dir = self.release_dir(version);
        std::fs::create_dir_all(&tag_dir).unwrap();
        let stage = self.path(&format!("stage-{version}"));
        std::fs::create_dir_all(&stage).unwrap();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"af {version}\"; exit 0; fi\necho \"fake af {version} $*\"\necho \"from=${{AF_DISPATCHED_FROM:-none}}\"\n"
        );
        write(&stage.join("af"), &script);
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(stage.join("af"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let asset = Self::asset(version);
        let status = Command::new("tar")
            .arg("-czf")
            .arg(tag_dir.join(&asset))
            .arg("-C")
            .arg(&stage)
            .arg("af")
            .status()
            .unwrap();
        assert!(status.success());
        let real = sha256_hex(&std::fs::read(tag_dir.join(&asset)).unwrap());
        let listed = if tamper {
            real.chars().rev().collect()
        } else {
            real.clone()
        };
        // A second target in the file, so a lock written from it records more than one digest.
        write(
            &tag_dir.join("SHA256SUMS"),
            &format!(
                "{listed}  {asset}\n{}  af-v{version}-other-target.tar.gz\n",
                "1".repeat(64)
            ),
        );
        real
    }

    /// Sign a published release's `SHA256SUMS` (with the file's bytes, or with other bytes to
    /// forge a signature that does not match).
    pub fn sign(&self, version: &str, signer: &Signer, over: Option<&[u8]>) {
        let sums = self.release_dir(version).join("SHA256SUMS");
        let bytes = match over {
            Some(bytes) => bytes.to_vec(),
            None => std::fs::read(&sums).unwrap(),
        };
        write(
            &self.release_dir(version).join("SHA256SUMS.minisig"),
            &signer.sign(&bytes),
        );
    }

    /// Adopt the real binary under test as an installed, receipted version.
    pub fn adopt_real_binary(&self) -> PathBuf {
        self.adopt_real_binary_with(&sha256_hex(&std::fs::read(AF).unwrap()))
    }

    /// Adopt the real binary with a receipt claiming `digest` for its archive — what a real
    /// install records from the release's `SHA256SUMS`.
    pub fn adopt_real_binary_with(&self, digest: &str) -> PathBuf {
        let dir = self.versions().join(VERSION);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(AF, dir.join("af")).unwrap();
        write(
            &dir.join("receipt.toml"),
            &format!(
                "version = \"{VERSION}\"\ntarget = \"{TARGET}\"\nsource = \"test\"\nasset = \"{}\"\nsha256 = \"{digest}\"\nverified_by = \"test\"\ninstalled_at = \"2026-09-03T00:00:00Z\"\n",
                Self::asset(VERSION)
            ),
        );
        dir.join("af")
    }

    /// A lock pinning `version`, with the archive digest for this target when given.
    pub fn lock(version: &str, digest: Option<&str>) -> String {
        let mut text = format!("version = 1\n\n[af]\nversion = \"{version}\"\n");
        if let Some(digest) = digest {
            text.push_str(&format!("\n[af.digests]\n{TARGET} = \"sha256:{digest}\"\n"));
        }
        text
    }

    /// A repository directory (a `.git` entry is all pin resolution needs) with the given lock.
    pub fn pinned_repo(&self, name: &str, lock: &str) -> PathBuf {
        let repo = self.path(name);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // A `.git` the kernel accepts as a repository holds a HEAD.
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        write(&repo.join(".af/af.lock"), lock);
        repo
    }
}

pub fn out(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn err(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
