//! `install.sh` against a fake `curl` serving a directory release source: every request pinned
//! to HTTPS, a refusal without `minisign` unless `AF_INSTALL_ALLOW_UNSIGNED=1` accepts the
//! checksum alone (said on stderr) that names the install line for the package manager, a re-run
//! that verifies the installed binary again rather than trusting its receipt, the rc channel, and
//! the next steps and PATH line an install ends with. Nothing here reaches the network.

use std::path::PathBuf;
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
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("install.sh")
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
        self.run_with_args(path, &[], env)
    }

    fn run_with_args(&self, path: &std::ffi::OsStr, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = std::process::Command::new(self.tools.join("sh"));
        command
            .arg(install_script())
            .args(args)
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

/// Finding I1: `install.sh` verifies a release over the anonymous route alone. With an empty
/// HOME and no `gh` on PATH, it lists, downloads and verifies through `curl` and installs.
#[test]
fn install_sh_needs_no_gh_when_curl_reaches_the_release() {
    let Some(host) = install_host() else {
        return;
    };
    let installer = Installer::new();
    let sandbox = &installer.sandbox;
    sandbox.publish_for("0.8.3", host);
    write(
        &sandbox.release_dir("0.8.3").join("SHA256SUMS.minisig"),
        "signature\n",
    );
    for directory in std::env::split_paths(&installer.with_minisign) {
        assert!(!directory.join("gh").exists(), "{}", directory.display());
    }
    assert!(
        std::fs::read_dir(sandbox.path("home"))
            .unwrap()
            .next()
            .is_none(),
        "an empty HOME"
    );
    let installed = installer.run(&installer.with_minisign, &[]);
    assert!(installed.status.success(), "{}", err(&installed));
    assert!(
        out(&installed).contains("af 0.8.3 installed at")
            && out(&installed).contains("minisign verified"),
        "{}",
        out(&installed)
    );
    assert!(!err(&installed).contains("gh"), "{}", err(&installed));
    let calls = std::fs::read_to_string(sandbox.path("curl.log")).unwrap();
    assert!(
        calls.contains("https://api.github.com/repos/owner/repo/releases?per_page=100")
            && calls.contains("releases/download/v0.8.3/SHA256SUMS.minisig"),
        "{calls}"
    );
}

/// The stdout lines of an install, which end it: at most six (ADR-0204).
fn ending(output: &Output) -> Vec<String> {
    let lines: Vec<String> = out(output).lines().map(str::to_owned).collect();
    assert!(lines.len() <= 6, "{lines:#?}");
    lines
}

#[test]
fn install_sh_ends_with_the_next_steps_and_the_path_line_for_the_shell() {
    let Some(host) = install_host() else {
        return;
    };
    let installer = Installer::new();
    let sandbox = &installer.sandbox;
    sandbox.publish_for("0.8.2", host);
    write(
        &sandbox.release_dir("0.8.2").join("SHA256SUMS.minisig"),
        "signature\n",
    );
    let bin = sandbox.path("bin");
    let next = [
        "next:",
        "  af provider detect   find and connect your claude/codex logins",
        "  af onboard            in a repository: preview .af/ (token-free)",
        "  af self setup-shell --write   completions + man pages",
    ];

    // fish, with the bin directory off PATH: its own PATH line.
    let fish = installer.run(&installer.with_minisign, &[("SHELL", "/usr/bin/fish")]);
    assert!(fish.status.success(), "{}", err(&fish));
    let lines = ending(&fish);
    assert!(lines[0].starts_with("af 0.8.2 installed at "), "{lines:#?}");
    assert_eq!(lines[1..5], next, "{lines:#?}");
    assert_eq!(
        lines[5],
        format!(
            "  fish_add_path {}   # af is not on PATH yet",
            bin.display()
        )
    );

    // bash: the export line, naming the rc file.
    let bash = installer.run(&installer.with_minisign, &[("SHELL", "/bin/bash")]);
    assert!(bash.status.success(), "{}", err(&bash));
    let lines = ending(&bash);
    assert!(lines[0].starts_with("af 0.8.2 is already installed at "));
    assert_eq!(lines[1..5], next, "{lines:#?}");
    assert_eq!(
        lines[5],
        format!(
            "  export PATH=\"{}:$PATH\"   # add to ~/.bashrc: af is not on PATH yet",
            bin.display()
        )
    );

    // Another shell gets the generic line.
    let other = installer.run(&installer.with_minisign, &[("SHELL", "/bin/ksh")]);
    assert_eq!(
        ending(&other)[5],
        format!("  add {} to PATH: af is not on PATH yet", bin.display())
    );

    // On PATH: no PATH line at all, whatever the shell.
    let mut on_path = std::env::split_paths(&installer.with_minisign).collect::<Vec<_>>();
    on_path.push(bin.clone());
    let on_path = std::env::join_paths(on_path).unwrap();
    for shell in ["/usr/bin/fish", "/bin/bash"] {
        let quiet = installer.run(&on_path, &[("SHELL", shell)]);
        assert!(quiet.status.success(), "{}", err(&quiet));
        let lines = ending(&quiet);
        assert_eq!(lines.len(), 5, "{lines:#?}");
        assert_eq!(lines[1..], next, "{lines:#?}");
        assert!(!out(&quiet).contains("PATH"), "{}", out(&quiet));
    }
}

#[test]
fn install_sh_installs_the_newest_pre_release_on_the_rc_channel() {
    let Some(host) = install_host() else {
        return;
    };
    let installer = Installer::new();
    let sandbox = &installer.sandbox;
    for version in ["0.8.2", "0.9.0-rc.2", "0.9.0-rc.10"] {
        sandbox.publish_for(version, host);
        write(
            &sandbox.release_dir(version).join("SHA256SUMS.minisig"),
            "signature\n",
        );
    }
    let path = &installer.with_minisign;

    // Stable by default: a pre-release is never chosen.
    let stable = installer.run(path, &[]);
    assert!(stable.status.success(), "{}", err(&stable));
    assert!(
        out(&stable).contains("af 0.8.2 installed at"),
        "{}",
        out(&stable)
    );

    // AF_INSTALL_CHANNEL=rc and --rc choose the newest pre-release, rc.10 over rc.2.
    let rc = installer.run(path, &[("AF_INSTALL_CHANNEL", "rc")]);
    assert!(rc.status.success(), "{}", err(&rc));
    assert!(
        out(&rc).contains("af 0.9.0-rc.10 installed at"),
        "{}",
        out(&rc)
    );
    std::fs::remove_dir_all(sandbox.path("data/af/versions/0.9.0-rc.10")).unwrap();
    let flag = installer.run_with_args(path, &["--rc"], &[]);
    assert!(flag.status.success(), "{}", err(&flag));
    assert!(
        out(&flag).contains("af 0.9.0-rc.10 installed at"),
        "{}",
        out(&flag)
    );
    let receipt = std::fs::read_to_string(
        installer
            .installed("0.9.0-rc.10")
            .with_file_name("receipt.toml"),
    )
    .unwrap();
    assert!(receipt.contains("version = \"0.9.0-rc.10\""), "{receipt}");

    // A release ranks above its own pre-releases.
    sandbox.publish_for("0.9.0", host);
    write(
        &sandbox.release_dir("0.9.0").join("SHA256SUMS.minisig"),
        "signature\n",
    );
    let released = installer.run(path, &[("AF_INSTALL_CHANNEL", "rc")]);
    assert!(
        out(&released).contains("af 0.9.0 installed at"),
        "{}",
        out(&released)
    );

    // Anything else is a usage error that says what to do.
    let unknown = installer.run(path, &[("AF_INSTALL_CHANNEL", "beta")]);
    assert_eq!(unknown.status.code(), Some(2), "{}", out(&unknown));
    assert!(
        err(&unknown).contains("AF_INSTALL_CHANNEL=rc"),
        "{}",
        err(&unknown)
    );
    let flag = installer.run_with_args(path, &["--beta"], &[]);
    assert_eq!(flag.status.code(), Some(2), "{}", out(&flag));
    assert!(err(&flag).contains("sh -s -- --rc"), "{}", err(&flag));
}

#[test]
fn install_sh_refusal_names_the_minisign_install_line_for_the_package_manager() {
    let Some(host) = install_host() else {
        return;
    };
    let installer = Installer::new();
    installer.sandbox.publish_for("0.8.2", host);
    for (manager, line) in [
        ("brew", "fix: brew install minisign, then run this again"),
        (
            "apt-get",
            "fix: sudo apt install minisign, then run this again",
        ),
    ] {
        let fake = installer.sandbox.path(&format!("fakes/{manager}"));
        executable(&fake.join(manager), "#!/bin/sh\nexit 1\n");
        let path = std::env::join_paths([&fake, &installer.tools]).unwrap();
        let refused = installer.run(&path, &[]);
        assert!(!refused.status.success(), "{}", out(&refused));
        assert!(err(&refused).contains(line), "{manager}: {}", err(&refused));
        assert!(!installer.installed("0.8.2").exists());
    }
}
