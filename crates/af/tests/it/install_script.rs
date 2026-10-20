//! `install.sh` against a fake `curl` serving a directory release source: every request pinned
//! to HTTPS, a refusal without `minisign` unless `AF_INSTALL_ALLOW_UNSIGNED=1` accepts the
//! checksum alone (said on stderr), and a re-run that verifies the installed binary again rather
//! than trusting its receipt. Nothing here reaches the network.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::common;
use common::{FAKE_CURL, Sandbox, err, executable, out, tool_dir, write};

/// The host name `install.sh` gives this machine, `None` where it builds no release.
fn install_host() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

fn install_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh")
}

/// The tools `install.sh` and the fake `curl` run; `shasum` is a Perl script on macOS.
const TOOLS: &[&str] = &[
    "sh",
    "uname",
    "mktemp",
    "rm",
    "tr",
    "sed",
    "awk",
    "grep",
    "sha256sum",
    "shasum",
    "perl",
    "mkdir",
    "tar",
    "gzip",
    "chmod",
    "cp",
    "mv",
    "cat",
    "date",
    "cmp",
    "basename",
];

/// A `minisign` that accepts any signature file that exists, logging every call.
const FAKE_MINISIGN: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_MINISIGN_LOG"
while [ $# -gt 0 ]; do
  case "$1" in -x) [ -f "$2" ] || exit 1; shift ;; esac
  shift
done
"#;

struct Installer {
    sandbox: Sandbox,
    tools: PathBuf,
    with_minisign: std::ffi::OsString,
}

impl Installer {
    fn new() -> Self {
        let sandbox = Sandbox::new();
        let tools = tool_dir(&sandbox.path("fakes"), TOOLS);
        executable(&tools.join("curl"), FAKE_CURL);
        let minisign = sandbox.path("fakes/minisign");
        executable(&minisign.join("minisign"), FAKE_MINISIGN);
        let with_minisign = std::env::join_paths([&minisign, &tools]).unwrap();
        Installer {
            sandbox,
            tools,
            with_minisign,
        }
    }

    fn run(&self, path: &std::ffi::OsStr, env: &[(&str, &str)]) -> Output {
        let mut command = std::process::Command::new(self.tools.join("sh"));
        command
            .arg(install_script())
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.sandbox.path("home"))
            .env("XDG_DATA_HOME", self.sandbox.path("data"))
            .env("XDG_BIN_HOME", self.sandbox.path("bin"))
            .env("AF_REPO", "owner/repo")
            .env("FAKE_RELEASES", self.sandbox.path("releases"))
            .env("FAKE_REPO", "owner/repo")
            .env("FAKE_CURL_LOG", self.sandbox.path("curl.log"))
            .env("FAKE_MINISIGN_LOG", self.sandbox.path("minisign.log"));
        for (name, value) in env {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn installed(&self, version: &str) -> PathBuf {
        self.sandbox.path(&format!("data/af/versions/{version}/af"))
    }
}

#[test]
fn install_sh_refuses_unsigned_pins_curl_to_https_and_verifies_again_on_a_rerun() {
    let Some(host) = install_host() else {
        return;
    };
    let installer = Installer::new();
    let sandbox = &installer.sandbox;
    sandbox.publish_for("0.8.1", host);
    sandbox.publish_for("0.8.2", host);
    write(
        &sandbox.release_dir("0.8.2").join("SHA256SUMS.minisig"),
        "signature\n",
    );
    let tools = installer.tools.as_os_str();

    // No minisign: refused, on stderr, and nothing installed.
    let refused = installer.run(tools, &[]);
    assert!(!refused.status.success(), "{}", out(&refused));
    assert!(
        err(&refused).contains("minisign is not installed")
            && err(&refused).contains("AF_INSTALL_ALLOW_UNSIGNED=1"),
        "{}",
        err(&refused)
    );
    assert!(!out(&refused).contains("minisign"), "{}", out(&refused));
    assert!(!installer.installed("0.8.2").exists());

    // Accepted explicitly: the checksum alone, said on stderr and in the receipt.
    let unsigned = installer.run(
        tools,
        &[
            ("AF_INSTALL_ALLOW_UNSIGNED", "1"),
            ("AF_INSTALL_VERSION", "0.8.1"),
        ],
    );
    assert!(unsigned.status.success(), "{}", err(&unsigned));
    assert!(
        err(&unsigned).contains("only the checksum is verified"),
        "{}",
        err(&unsigned)
    );
    assert!(
        !out(&unsigned).contains("only the checksum"),
        "{}",
        out(&unsigned)
    );
    let receipt =
        std::fs::read_to_string(installer.installed("0.8.1").with_file_name("receipt.toml"))
            .unwrap();
    assert!(
        receipt.contains("verified_by = \"sha256sums\""),
        "{receipt}"
    );

    // With minisign, the newest stable release, its signature checked.
    let signed = installer.run(&installer.with_minisign, &[]);
    assert!(signed.status.success(), "{}", err(&signed));
    assert!(
        out(&signed).contains("af 0.8.2 installed at")
            && out(&signed).contains("minisign verified"),
        "{}",
        out(&signed)
    );
    let released = std::fs::read(installer.installed("0.8.2")).unwrap();

    // A re-run verifies again: an intact install is kept ...
    let again = installer.run(&installer.with_minisign, &[]);
    assert!(again.status.success(), "{}", err(&again));
    assert!(
        out(&again).contains("af 0.8.2 is already installed")
            && out(&again).contains("verified again"),
        "{}",
        out(&again)
    );
    // ... and one whose bytes changed under its receipt is replaced with the release's.
    write(&installer.installed("0.8.2"), "#!/bin/sh\necho planted\n");
    let replaced = installer.run(&installer.with_minisign, &[]);
    assert!(replaced.status.success(), "{}", err(&replaced));
    assert!(
        out(&replaced).contains("af 0.8.2 installed at"),
        "{}",
        out(&replaced)
    );
    assert_eq!(
        std::fs::read(installer.installed("0.8.2")).unwrap(),
        released
    );

    // Every request was pinned to HTTPS; the fake refuses any other.
    let calls = std::fs::read_to_string(sandbox.path("curl.log")).unwrap();
    assert!(!calls.is_empty());
    for call in calls.lines() {
        assert!(
            call.contains("--proto =https --proto-redir =https --tlsv1.2"),
            "{call}"
        );
    }
    assert!(
        std::fs::read_to_string(sandbox.path("minisign.log"))
            .unwrap()
            .contains("-V"),
        "minisign was not asked"
    );
}
