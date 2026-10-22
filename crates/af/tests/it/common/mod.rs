//! A sandboxed self-managed layout for tests: XDG directories in a temporary root, a directory
//! release source, fake releases whose `af` is a shell script, optional minisign signatures made
//! the way the release job makes them, and the real binary adopted as a receipted install.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "../../../../review-process/tests/support/executables.rs"]
mod executables;

/// The binary under test as this test process starts it: its own link to the `af` cargo built
/// for these tests, taken under the boundary every test builder and launcher of a workspace
/// output shares (ADR-0174). `review-pipeline`'s tests build `af` again, and every cargo build
/// replaces `CARGO_BIN_EXE_af` (on macOS it removes and copies the file), so starting that path
/// itself could meet a missing file; the link is the same inode, whatever replaces the path.
pub fn af_binary() -> &'static Path {
    static HELD: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HELD.get_or_init(|| executables::held(Path::new(env!("CARGO_BIN_EXE_af"))))
}
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TARGET: &str = env!("AF_TARGET");

/// `af` for a test: the binary under test with `HOME` and every XDG directory in a private
/// directory of this test process, below cargo's target directory, so no test reads or changes
/// the developer's real af state. A run ends with the Storage Budget's sweep of everything af
/// keeps on the machine (ADR-0144), so a test that reached the real `~/.local/state/af` could
/// remove a developer's own history. The toolchain stays reachable: rustup hands every process
/// it starts `RUSTUP_HOME` and `CARGO_HOME`. A test that sets any of these names itself
/// afterwards wins, since the last `env` of a name counts.
///
/// It also raises the provider probes' timeouts to [`LOAD_SAFE_PROBE_TIMEOUT_MS`], shortens the
/// in-Attempt retry's first wait to [`TEST_RETRY_BASE_MS`], and sets `AF_SELF_OFFLINE`: an
/// eligible command spawns the detached update check whether or not it has a terminal, and that
/// check would ask the real release source over the network. Tests of the check use
/// [`Sandbox::command`], whose release source is a directory.
pub fn af() -> Command {
    let mut command = Command::new(af_binary());
    for (name, value) in private_home_env() {
        command.env(name, value);
    }
    command.env(PROBE_TIMEOUT_SETTING, LOAD_SAFE_PROBE_TIMEOUT_MS);
    command.env(RETRY_BASE_SETTING, TEST_RETRY_BASE_MS);
    command.env("AF_SELF_OFFLINE", "1");
    command
}

/// The debug-only setting of the first wait before an Attempt calls its Provider again after a
/// transient failure (ADR-0196); a release binary always waits its production 60 s.
pub const RETRY_BASE_SETTING: &str = "AF_TEST_PROVIDER_RETRY_BASE_MS";

/// Ten milliseconds in place of the production minute: a fake Provider's transient failure is
/// retried as in production, without a test waiting minutes for it. A test whose subject is the
/// wait sets its own.
pub const TEST_RETRY_BASE_MS: &str = "10";

/// The debug-only setting that raises the provider status, subscription, version and usage
/// probes' timeouts (ADR-0114); a release binary never reads it.
pub const PROBE_TIMEOUT_SETTING: &str = "AF_TEST_PROVIDER_PROBE_TIMEOUT_MS";

/// Two minutes, the load-safe wall (ADR-0114), in place of the production 15 s Codex and 10 s
/// Claude usage probes. Starting a fake provider's shell or Python on a loaded gate (seven
/// nextest threads, load 7-28) took about 5 s, so a test whose subject is not the probe's
/// timeout raced scheduling and reported a provider af could not tell about. A probe that
/// answers still ends when it answers, and an Attempt deadline still bounds every probe; only a
/// hung fixture waits longer, and only to fail. A test whose subject is a probe timeout removes
/// the setting to keep the production value; none of the `af` integration tests is one.
pub const LOAD_SAFE_PROBE_TIMEOUT_MS: &str = "120000";

/// `HOME` and the XDG directories of this test process's private home, created once.
pub fn private_home_env() -> Vec<(&'static str, PathBuf)> {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let home = HOME.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let home = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("af-test-homes")
            .join(format!("{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        home
    });
    vec![
        ("HOME", home.clone()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_STATE_HOME", home.join(".local/state")),
        ("XDG_CACHE_HOME", home.join(".cache")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_BIN_HOME", home.join(".local/bin")),
    ]
}

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
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"af {version}\"; exit 0; fi\necho \"fake af {version} $*\"\necho \"from=${{AF_DISPATCHED_FROM:-none}}\"\n"
        );
        self.publish_script(version, &script, tamper)
    }

    /// A release like [`Sandbox::publish`]'s whose `af --version`, the proof an installer runs
    /// after fetching and verifying the archive, stalls the install: it creates `stall/reached`,
    /// then waits for the test to create `stall/open`. The wait is bounded: after a minute it
    /// creates `stall/expired` and fails, so a broken test never leaves an installer behind.
    pub fn publish_stalled(&self, version: &str, stall: &Path) -> String {
        std::fs::create_dir_all(stall).unwrap();
        let (reached, open, expired) = (
            stall.join("reached"),
            stall.join("open"),
            stall.join("expired"),
        );
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  : > '{reached}'\n  i=0\n  while [ ! -e '{open}' ]; do\n    i=$((i + 1))\n    if [ \"$i\" -gt 600 ]; then : > '{expired}'; exit 1; fi\n    sleep 0.1\n  done\n  echo \"af {version}\"\n  exit 0\nfi\necho \"fake af {version} $*\"\n",
            reached = reached.display(),
            open = open.display(),
            expired = expired.display(),
        );
        self.publish_script(version, &script, false)
    }

    fn publish_script(&self, version: &str, script: &str, tamper: bool) -> String {
        self.publish_script_for(version, TARGET, script, tamper)
    }

    /// [`Sandbox::publish`] for the target `target`, as `install.sh` names its host.
    pub fn publish_for(&self, version: &str, target: &str) -> String {
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"af {version}\"; exit 0; fi\necho \"fake af {version} $*\"\n"
        );
        self.publish_script_for(version, target, &script, false)
    }

    fn publish_script_for(
        &self,
        version: &str,
        target: &str,
        script: &str,
        tamper: bool,
    ) -> String {
        let tag_dir = self.release_dir(version);
        std::fs::create_dir_all(&tag_dir).unwrap();
        let stage = self.path(&format!("stage-{version}"));
        std::fs::create_dir_all(&stage).unwrap();
        write(&stage.join("af"), script);
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(stage.join("af"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let asset = format!("af-v{version}-{target}.tar.gz");
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
        self.adopt_real_binary_with(&sha256_hex(&std::fs::read(af_binary()).unwrap()))
    }

    /// Adopt the real binary with a receipt claiming `digest` for its archive — what a real
    /// install records from the release's `SHA256SUMS`.
    pub fn adopt_real_binary_with(&self, digest: &str) -> PathBuf {
        let dir = self.versions().join(VERSION);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(af_binary(), dir.join("af")).unwrap();
        write(
            &dir.join("receipt.toml"),
            &format!(
                "version = \"{VERSION}\"\ntarget = \"{TARGET}\"\nsource = \"test\"\nasset = \"{}\"\nsha256 = \"{digest}\"\nverified_by = \"test\"\ninstalled_at = \"2026-09-03T00:00:00Z\"\n",
                Self::asset(VERSION)
            ),
        );
        dir.join("af")
    }

    /// `binary` running `af self refresh-check` as a command schedules it: the command creates
    /// the check lock's file first, which the check never creates itself.
    pub fn refresh_check(&self, binary: &Path) -> Command {
        let lock = self.path("cache/af/self/latest.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&lock)
            .unwrap();
        let mut command = self.command(binary);
        command.args(["self", "refresh-check"]);
        command
    }

    /// `[self] install_pins = <value>` as the whole machine configuration: `"always"` installs a
    /// lock's pin on demand without the terminal confirmation the default `"ask"` needs.
    pub fn install_pins(&self, value: &str) {
        write(
            &self.path("config/af/config.toml"),
            &format!("[self]\ninstall_pins = {value}\n"),
        );
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

/// A directory holding links to `tools` as the real `PATH` finds them, those it has, and nothing
/// else: as `PATH`, it has no `gh`, `minisign` or `curl` unless a test puts one there.
pub fn tool_dir(root: &Path, tools: &[&str]) -> PathBuf {
    let dir = root.join("tools");
    std::fs::create_dir_all(&dir).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    for tool in tools {
        if let Some(found) = std::env::split_paths(&path)
            .map(|dir| dir.join(tool))
            .find(|candidate| candidate.is_file())
        {
            std::os::unix::fs::symlink(found, dir.join(tool)).unwrap();
        }
    }
    dir
}

/// Write `script` at `path` and make it executable.
pub fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, script);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `curl` that answers as GitHub would from the release directory `$FAKE_RELEASES` for the
/// repository `$FAKE_REPO`, or with `$FAKE_CURL_STATUS` for everything; it logs every call to
/// `$FAKE_CURL_LOG` and refuses one not pinned to HTTPS. It needs `cp` and `basename`.
pub const FAKE_CURL: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_CURL_LOG"
out=""; url=""; proto=""; redir=""; tls=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    -w|-H|--retry) shift ;;
    --proto) proto="$2"; shift ;;
    --proto-redir) redir="$2"; shift ;;
    --tlsv1.2) tls=1 ;;
    https://*) url="$1" ;;
  esac
  shift
done
[ "$proto" = "=https" ] && [ "$redir" = "=https" ] && [ -n "$tls" ] || { echo "not pinned to https" >&2; exit 2; }
if [ -n "${FAKE_CURL_STATUS:-}" ]; then printf '%s' "$FAKE_CURL_STATUS"; exit 0; fi
case "$url" in
  "https://api.github.com/repos/$FAKE_REPO/releases?per_page=100")
    sep=""; printf '[' > "$out"
    for dir in "$FAKE_RELEASES"/*/; do
      tag=$(basename "$dir")
      case "$tag" in *-*) pre=true ;; *) pre=false ;; esac
      printf '%s{"tag_name":"%s","prerelease":%s,"draft":false}' "$sep" "$tag" "$pre" >> "$out"
      sep=","
    done
    printf ']' >> "$out"
    printf 200 ;;
  "https://github.com/$FAKE_REPO/releases/download/"*)
    rest=${url#"https://github.com/$FAKE_REPO/releases/download/"}
    if [ -f "$FAKE_RELEASES/$rest" ]; then cp "$FAKE_RELEASES/$rest" "$out"; printf 200; else printf 404; fi ;;
  *) printf 404 ;;
esac
"#;

/// A `gh` that serves `release list` and `release download` from `$FAKE_RELEASES`, logging
/// every call to `$FAKE_GH_LOG`.
pub const FAKE_GH: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GH_LOG"
case "$1 $2" in
  "release list")
    sep=""; printf '['
    for dir in "$FAKE_RELEASES"/*/; do
      tag=$(basename "$dir")
      case "$tag" in *-*) pre=true ;; *) pre=false ;; esac
      printf '%s{"tagName":"%s","isPrerelease":%s,"isDraft":false}' "$sep" "$tag" "$pre"
      sep=","
    done
    printf ']\n' ;;
  "release download")
    tag="$3"; shift 3; pattern=""; dir=""
    while [ $# -gt 0 ]; do
      case "$1" in --pattern) pattern="$2"; shift ;; --dir) dir="$2"; shift ;; esac
      shift
    done
    [ -f "$FAKE_RELEASES/$tag/$pattern" ] || { echo "release not found" >&2; exit 1; }
    cp "$FAKE_RELEASES/$tag/$pattern" "$dir/$pattern" ;;
  *) echo "unexpected gh $*" >&2; exit 1 ;;
esac
"#;

pub fn out(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn err(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// An empty directory in `parent` (created when absent) named as a removal of process `pid`
/// claims it, `.af-removing-<pid>-<device>-<inode>`, binding its own device and inode: a claim
/// af can prove it made (ADR-0144), unlike a name that is only claim-shaped.
pub fn af_claim(parent: &Path, pid: u32) -> PathBuf {
    std::fs::create_dir_all(parent).unwrap();
    let made = tempfile::Builder::new()
        .prefix("claimed-")
        .tempdir_in(parent)
        .unwrap()
        .keep();
    let identity = review_sandbox::Identity::of(&made).unwrap();
    let claim = parent.join(review_sandbox::claim_name(pid, identity));
    std::fs::rename(&made, &claim).unwrap();
    claim
}
