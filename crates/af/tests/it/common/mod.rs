//! A sandboxed self-managed layout for tests: XDG directories in a temporary root, a directory
//! release source, fake releases whose `af` is a shell script, optional minisign signatures made
//! the way the release job makes them, and the real binary adopted as a receipted install.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "../../../../review-process/tests/support/executables.rs"]
mod executables;

/// A process a test only read the ID of, signalled only while that ID still names it (issue
/// #331 item 7).
#[path = "../../../../review-process/tests/support/recorded.rs"]
pub mod recorded;

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
/// The commit every fake nightly's `af --version --json` reports.
pub const NIGHTLY_COMMIT: &str = "0123456789ab";

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
/// [`Sandbox::command`], whose release source is a directory. And it empties
/// [`DETECT_SYSTEM_DIRS_SETTING`], so `af provider detect` never finds the developer's own
/// `/opt/homebrew/bin/claude` or `/usr/local/bin/codex`; a test of those directories names
/// private ones.
pub fn af() -> Command {
    let mut command = Command::new(af_binary());
    for (name, value) in private_home_env() {
        command.env(name, value);
    }
    command.env(PROBE_TIMEOUT_SETTING, LOAD_SAFE_PROBE_TIMEOUT_MS);
    command.env(DETECT_SYSTEM_DIRS_SETTING, "");
    command.env(RETRY_BASE_SETTING, TEST_RETRY_BASE_MS);
    command.env("AF_SELF_OFFLINE", "1");
    // Human text is held to `COLUMNS` without a terminal, as clap holds `--help`: the
    // developer's own terminal width never changes what a test reads. A test of a width sets
    // its own.
    command.env_remove("COLUMNS");
    command
}

/// The debug-only setting that replaces the system install directories `af provider detect`
/// searches (`/opt/homebrew/bin`, `/usr/local/bin`) with a `:`-separated list; a release binary
/// never reads it (ADR-0206).
pub const DETECT_SYSTEM_DIRS_SETTING: &str = "AF_TEST_DETECT_SYSTEM_DIRS";

/// The debug-only setting of the first wait before an Attempt calls its Provider again after a
/// transient failure (ADR-0208); a release binary always waits its production 60 s.
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
        // Resolved, as af resolves the paths it reports: a target directory reached through a
        // symlink (macOS's `/var` and `/tmp`, a warm layer's clone in a check's runtime
        // directory) would otherwise make every path assertion compare two spellings.
        std::fs::canonicalize(&home).unwrap()
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

/// This test process's own `TMPDIR`, for a child a helper spawns after `env_clear()`: without
/// it the child falls back to the system's temporary directory (`/var/folders/…` on macOS),
/// which a sandbox whose `TMPDIR` is private forbids. `None` when this process has none.
pub fn inherited_tmpdir() -> Option<(&'static str, std::ffi::OsString)> {
    std::env::var_os("TMPDIR").map(|tmpdir| ("TMPDIR", tmpdir))
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
            .env_remove("AF_NIGHTLY_KEY")
            .env_remove("AF_TEST_EMBEDDED_NIGHTLY_KEY")
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
        self.publish_tag(&format!("v{version}"), target, script, tamper)
    }

    /// A nightly tagged `tag` (`nightly-YYYY.MM.DD`) for `target`, whose `af` answers
    /// `--version --json` with `version` (`<core>-nightly.<YYYYMMDD>`) and [`NIGHTLY_COMMIT`],
    /// in the release layout with an unsigned `SHA256SUMS`. Returns the archive's digest.
    pub fn publish_nightly_for(&self, tag: &str, version: &str, target: &str) -> String {
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  if [ \"$2\" = \"--json\" ]; then printf '{{\"version\": \"{version}\", \"commit\": \"{NIGHTLY_COMMIT}\", \"target\": \"{target}\"}}\\n'; exit 0; fi\n  echo \"af {version}\"; exit 0\nfi\necho \"fake af {version} $*\"\n"
        );
        self.publish_tag(tag, target, &script, false)
    }

    /// [`Sandbox::publish_nightly_for`] this machine's target.
    pub fn publish_nightly(&self, tag: &str, version: &str) -> String {
        self.publish_nightly_for(tag, version, TARGET)
    }

    /// A nightly tagged `tag` for this machine's target whose `af` is the binary under test: run
    /// with `AF_TEST_RUNNING_VERSION` set to `version`, it reports that version as its own, so
    /// the nightly a switch installs from it is the af that then runs as that nightly. Returns
    /// the archive's digest.
    pub fn publish_nightly_binary(&self, tag: &str) -> String {
        self.publish_tag_with(tag, TARGET, false, |af| copy_by_a_child(private_af(), af))
    }

    /// `releases/<tag>/af-<tag>-<target>.tar.gz` holding `script` as `af`, beside a
    /// `SHA256SUMS` that lists it (tampered when asked) and a second target.
    fn publish_tag(&self, tag: &str, target: &str, script: &str, tamper: bool) -> String {
        self.publish_tag_with(tag, target, tamper, |af| write(af, script))
    }

    /// [`Sandbox::publish_tag`] with `af` written by `stage`, which gets its path.
    fn publish_tag_with(
        &self,
        tag: &str,
        target: &str,
        tamper: bool,
        stage: impl FnOnce(&Path),
    ) -> String {
        let tag_dir = self.path(&format!("releases/{tag}"));
        std::fs::create_dir_all(&tag_dir).unwrap();
        let staging = self.path(&format!("stage-{tag}"));
        std::fs::create_dir_all(&staging).unwrap();
        let _ = std::fs::remove_file(staging.join("af"));
        stage(&staging.join("af"));
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(staging.join("af"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let asset = format!("af-{tag}-{target}.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(tag_dir.join(&asset))
            .arg("-C")
            .arg(&staging)
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
                "{listed}  {asset}\n{}  af-{tag}-other-target.tar.gz\n",
                "1".repeat(64)
            ),
        );
        real
    }

    /// Sign a published release's `SHA256SUMS` (with the file's bytes, or with other bytes to
    /// forge a signature that does not match).
    pub fn sign(&self, version: &str, signer: &Signer, over: Option<&[u8]>) {
        self.sign_tag(&format!("v{version}"), signer, over);
    }

    /// [`Sandbox::sign`] for the release tagged `tag`, a nightly's among them.
    pub fn sign_tag(&self, tag: &str, signer: &Signer, over: Option<&[u8]>) {
        let dir = self.path(&format!("releases/{tag}"));
        let bytes = match over {
            Some(bytes) => bytes.to_vec(),
            None => std::fs::read(dir.join("SHA256SUMS")).unwrap(),
        };
        write(&dir.join("SHA256SUMS.minisig"), &signer.sign(&bytes));
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
        install_private_af(&dir.join("af"));
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

/// Write `script` at `path` and make it executable. A single-threaded writer process writes it
/// (see [`written_by_a_child`]), so a test that starts the script at once never meets `Text
/// file busy`.
pub fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    written_by_a_child(path, script.as_bytes());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Write `bytes` to `path` through a single-threaded child process, which this process reaps
/// before it returns. Linux refuses to start a file that any process holds open for writing
/// (`ETXTBSY`, `Text file busy`, issue #121), and a child another test thread forks while this
/// multi-threaded process holds such a descriptor inherits it until its own `exec`. Under
/// `cargo test`, which runs a binary's tests as threads of one process, a script a test wrote
/// and started at once failed that way; under nextest, one process per test, it never did. The
/// descriptor here lives only in the writer, so no sibling can inherit it.
pub fn written_by_a_child(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "exec cat > \"$1\"", "fixture-writer"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    writer.stdin.take().unwrap().write_all(bytes).unwrap();
    let output = writer.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "writing {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// This process's one private copy of the binary under test, made once, by a child process
/// (see [`written_by_a_child`]): a test that installs `af` somewhere links this copy instead
/// of copying the binary again, so no later install leaves a written file behind for a sibling
/// thread's child to hold open. It is a copy, not a link to [`af_binary`], so nothing a test
/// does to an installed `af` can reach the file cargo built.
pub fn private_af() -> &'static Path {
    static COPY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    COPY.get_or_init(|| {
        let home = private_home_env()
            .into_iter()
            .find(|(name, _)| *name == "HOME")
            .map(|(_, home)| home)
            .unwrap();
        let directory = home.with_extension("binaries");
        std::fs::create_dir_all(&directory).unwrap();
        let copy = directory.join("af");
        copy_by_a_child(af_binary(), &copy);
        copy
    })
}

/// Install the binary under test at `path`: a hard link to [`private_af`], or where the
/// filesystem refuses one, a copy a child process makes.
pub fn install_private_af(path: &Path) {
    install_private_af_with_link(path, |source, destination| {
        std::fs::hard_link(source, destination)
    });
}

// Keep the fallback exercisable even when the test machine has only one filesystem.
fn install_private_af_with_link(
    path: &Path,
    link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) {
    if link(private_af(), path).is_err() {
        copy_by_a_child(private_af(), path);
    }
}

/// `cp -p source destination` in a child process: its mode and bytes, and no writable
/// descriptor in this one.
fn copy_by_a_child(source: &Path, destination: &Path) {
    let output = Command::new("/bin/sh")
        .args(["-c", "exec cp -p \"$1\" \"$2\"", "fixture-copier"])
        .arg(source)
        .arg(destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "copying {} to {}: {}",
        source.display(),
        destination.display(),
        String::from_utf8_lossy(&output.stderr)
    );
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

/// Whether a printed `line` keeps to `width` columns, or is one unbreakable unit on a line of
/// its own: a path or a word, or a `--flag value` pair, indented and continued with ` \`. A
/// path wider than the terminal (a long `TMPDIR`) cannot be broken without breaking the copied
/// command, so af gives it a line to itself instead.
pub fn fits_or_unbreakable(line: &str, width: usize) -> bool {
    if line.chars().count() <= width {
        return true;
    }
    let unit = line.trim().trim_end_matches('\\').trim_end();
    match shell_words::split(unit).as_deref() {
        Ok([_]) => true,
        Ok([flag, _]) => flag.starts_with("--"),
        _ => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #121: under `cargo test` every test of this binary is a thread of one process, and
    /// a script one thread wrote and started at once failed with `Text file busy` whenever
    /// another thread had just forked. Eight threads each write a fresh script and start it
    /// at once, 40 times, while four more spawn processes the whole time; every start succeeds.
    #[test]
    fn scripts_written_while_other_threads_spawn_start_without_text_file_busy() {
        let directory = tempfile::tempdir().unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spawners: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        Command::new("/bin/sh").args(["-c", ":"]).status().unwrap();
                    }
                })
            })
            .collect();
        let writers: Vec<_> = (0..8)
            .map(|thread| {
                let root = directory.path().to_path_buf();
                std::thread::spawn(move || {
                    for round in 0..40 {
                        let script = root.join(format!("script-{thread}-{round}"));
                        executable(&script, &format!("#!/bin/sh\necho {thread}-{round}\n"));
                        let output = Command::new(&script).output().unwrap_or_else(|error| {
                            panic!("starting {}: {error}", script.display())
                        });
                        assert_eq!(out(&output), format!("{thread}-{round}\n"));
                    }
                })
            })
            .collect();
        let results: Vec<_> = writers.into_iter().map(|writer| writer.join()).collect();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for spawner in spawners {
            spawner.join().unwrap();
        }
        for result in results {
            result.unwrap();
        }
    }

    /// The binary under test is copied once per process: same-filesystem installs link that
    /// one copy, which holds exactly the built bytes, keeps its mode and starts.
    #[test]
    fn the_binary_under_test_is_copied_once_per_process() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // TMPDIR may be a different mount from cargo's target (issue #349).
        let directory = tempfile::tempdir_in(private_af().parent().unwrap()).unwrap();
        let (first, second) = (directory.path().join("one"), directory.path().join("two"));
        install_private_af(&first);
        install_private_af(&second);
        assert_eq!(private_af(), private_af());
        let copy = std::fs::metadata(private_af()).unwrap();
        for installed in [&first, &second] {
            let metadata = std::fs::metadata(installed).unwrap();
            assert_eq!((metadata.dev(), metadata.ino()), (copy.dev(), copy.ino()));
            assert_eq!(metadata.permissions().mode() & 0o111, 0o111);
            assert_installed_binary(installed);
            eprintln!(
                "hard-link install: private=({},{}) installed=({},{})",
                copy.dev(),
                copy.ino(),
                metadata.dev(),
                metadata.ino()
            );
        }
        let held = std::fs::metadata(af_binary()).unwrap();
        assert_ne!(
            (copy.dev(), copy.ino()),
            (held.dev(), held.ino()),
            "the private copy is not the file cargo built"
        );
    }

    /// Force the supported link-refusal route on every platform, through the same installer
    /// and intentional child copy as a real cross-device destination.
    #[test]
    fn the_binary_under_test_is_copied_when_a_hard_link_is_refused() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir_in(private_af().parent().unwrap()).unwrap();
        let installed = directory.path().join("af");
        let mut attempted = false;
        install_private_af_with_link(&installed, |source, destination| {
            assert_eq!(source, private_af());
            assert_eq!(destination, installed);
            attempted = true;
            Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices))
        });
        assert!(attempted, "the installer must attempt the refused link");
        let copy = std::fs::metadata(&installed).unwrap();
        for source in [private_af(), af_binary()] {
            let metadata = std::fs::metadata(source).unwrap();
            assert_ne!(
                (copy.dev(), copy.ino()),
                (metadata.dev(), metadata.ino()),
                "the fallback copy is isolated from {}",
                source.display()
            );
            eprintln!(
                "forced CrossesDevices fallback: source=({},{}) installed=({},{})",
                metadata.dev(),
                metadata.ino(),
                copy.dev(),
                copy.ino()
            );
        }
        assert_installed_binary(&installed);
    }

    fn assert_installed_binary(installed: &Path) {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::read(installed).unwrap(),
            std::fs::read(af_binary()).unwrap()
        );
        let mode = |path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(installed), mode(private_af()));
        assert_eq!(mode(installed), mode(af_binary()));
        assert_eq!(mode(installed) & 0o111, 0o111);
        let output = Command::new(installed)
            .envs(private_home_env())
            .env("AF_SELF_OFFLINE", "1")
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", err(&output));
        assert_eq!(out(&output).trim(), format!("af {VERSION}"));
    }
}
