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

/// The stdout lines of an install, which end it: at most six (ADR-0210).
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
    // The Provider step is the README's: it registers what it finds, where `af provider detect`
    // alone only lists (ADR-0206). Every description starts in one column.
    let next = [
        "next:",
        "  af provider detect --apply   find your claude/codex logins and register them",
        "  af onboard                   in a repository: preview .af/ (token-free)",
        "  af self setup-shell --write  completions + man pages",
    ];
    let readme = std::fs::read_to_string(install_script().with_file_name("README.md")).unwrap();
    assert!(
        readme.contains("```sh\n   af provider detect --apply\n   ```"),
        "the README's Provider step"
    );
    let description_column = |line: &str| {
        let command_end = line[2..].find("  ").unwrap() + 2;
        command_end + line[command_end..].len() - line[command_end..].trim_start().len()
    };
    assert!(
        next[1..]
            .iter()
            .all(|line| description_column(line) == description_column(next[1])),
        "{next:#?}"
    );

    // fish, with the bin directory off PATH: its own PATH line, the directory a fish literal.
    let fish = installer.run(&installer.with_minisign, &[("SHELL", "/usr/bin/fish")]);
    assert!(fish.status.success(), "{}", err(&fish));
    let lines = ending(&fish);
    assert!(lines[0].starts_with("af 0.8.2 installed at "), "{lines:#?}");
    assert_eq!(lines[1..5], next, "{lines:#?}");
    assert_eq!(
        lines[5],
        format!(
            "  fish_add_path '{}'   # af is not on PATH yet",
            bin.display()
        )
    );

    // A bin directory with a space, a `'` and a `\`: fish reads its line as one path argument,
    // the `'` and `\` escaped the way `af doctor` writes them; bash's single quotes keep the
    // space and the `\`, the `'` written `'\''`. Neither line is run.
    let spaced = sandbox.path("My Tools/it's\\bin");
    let spaced_env = spaced.to_str().unwrap();
    let fish = installer.run(
        &installer.with_minisign,
        &[("SHELL", "/usr/bin/fish"), ("XDG_BIN_HOME", spaced_env)],
    );
    assert!(fish.status.success(), "{}", err(&fish));
    let lines = ending(&fish);
    assert_eq!(lines[1..5], next, "{lines:#?}");
    assert_eq!(
        lines[5],
        format!(
            "  fish_add_path '{}/My Tools/it\\'s\\\\bin'   # af is not on PATH yet",
            spaced.ancestors().nth(2).unwrap().display()
        ),
        "{lines:#?}"
    );
    let bash = installer.run(
        &installer.with_minisign,
        &[("SHELL", "/bin/bash"), ("XDG_BIN_HOME", spaced_env)],
    );
    assert!(bash.status.success(), "{}", err(&bash));
    assert_eq!(
        ending(&bash)[5],
        format!(
            "  export PATH='{}/My Tools/it'\\''s\\bin':\"$PATH\"   # add to ~/.bashrc: af is not on \
             PATH yet",
            spaced.ancestors().nth(2).unwrap().display()
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
            "  export PATH='{}':\"$PATH\"   # add to ~/.bashrc: af is not on PATH yet",
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

/// The directory a POSIX shell word of single-quoted runs and `\`-escaped characters spells, and
/// the text after it; `None` where the word leaves a quote open. Nothing is run to read it.
fn posix_word(line: &str) -> Option<(String, &str)> {
    let mut word = String::new();
    let mut rest = line;
    loop {
        if let Some(quoted) = rest.strip_prefix('\'') {
            let end = quoted.find('\'')?;
            word.push_str(&quoted[..end]);
            rest = &quoted[end + 1..];
        } else if let Some(escaped) = rest.strip_prefix('\\') {
            let c = escaped.chars().next()?;
            word.push(c);
            rest = &escaped[c.len_utf8()..];
        } else {
            return Some((word, rest));
        }
    }
}

/// Issue #358: the bash and zsh lines keep a bin directory literal when pasted, as `af doctor`'s
/// do. A `$HOME`, a `$(…)`, a backtick, `"`, `'` and `\` in it are single-quoted, each `'` written
/// `'\''`, and only the `$PATH` after the directory expands. The lines are read, and parsed by
/// `sh -n`, never run.
#[test]
fn install_sh_bash_and_zsh_path_lines_keep_the_directory_literal() {
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
    let hostile = sandbox.path("h/$HOME/$(touch x)/`id`/\"q\"/it's\\bin");
    let hostile_env = hostile.to_str().unwrap();
    let root = hostile.ancestors().nth(6).unwrap().display().to_string();
    assert_eq!(format!("{root}/h"), sandbox.path("h").display().to_string());
    for (shell, rc) in [("/bin/bash", "~/.bashrc"), ("/usr/bin/zsh", "~/.zshrc")] {
        let install = installer.run(
            &installer.with_minisign,
            &[("SHELL", shell), ("XDG_BIN_HOME", hostile_env)],
        );
        assert!(install.status.success(), "{}", err(&install));
        let lines = ending(&install);
        let tail = format!(":\"$PATH\"   # add to {rc}: af is not on PATH yet");
        assert_eq!(
            lines[5],
            format!("  export PATH='{root}/h/$HOME/$(touch x)/`id`/\"q\"/it'\\''s\\bin'{tail}"),
            "{lines:#?}"
        );
        let command = lines[5].trim_start().split("   # ").next().unwrap();
        let (directory, rest) = posix_word(command.strip_prefix("export PATH=").unwrap()).unwrap();
        assert_eq!(directory, hostile_env, "{command}");
        assert_eq!(rest, ":\"$PATH\"", "{command}");
        let parsed = std::process::Command::new("/bin/sh")
            .args(["-n", "-c", command])
            .output()
            .unwrap();
        assert!(parsed.status.success(), "{command}: {parsed:?}");
    }
}

/// Issue #358: no PATH entry holds a bin directory with a `:` or a control character, as `af
/// doctor` judges it, so the install ends with no command for any shell: one line that says so
/// and to choose another `XDG_BIN_HOME`, the directory shown only when it has no control
/// character. A `:` is judged before PATH is asked, so a PATH that spells the directory across two
/// entries does not hide it. Nothing printed is run.
#[test]
fn install_sh_gives_no_path_line_for_a_bin_directory_no_path_entry_can_hold() {
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
    let no_command = |lines: &[String]| {
        for line in lines {
            assert!(
                !line.contains("export PATH")
                    && !line.contains("fish_add_path")
                    && !line.contains(" to PATH"),
                "{lines:#?}"
            );
        }
    };
    let shells = ["/bin/bash", "/usr/bin/zsh", "/usr/bin/fish", "/bin/ksh"];

    // A `:`: the directory is shown, with the fix, and no shell gets a command.
    let colon = sandbox.path("af:bin");
    let colon_env = colon.to_str().unwrap();
    let refusal = format!(
        "  no PATH entry can hold {colon_env}, which has a `:`: set XDG_BIN_HOME to a directory \
         without one, then install again"
    );
    let mut spelled = std::env::split_paths(&installer.with_minisign).collect::<Vec<_>>();
    spelled.push(sandbox.path("af"));
    spelled.push(PathBuf::from("bin"));
    let spelled = std::env::join_paths(spelled).unwrap();
    assert!(
        format!(":{}:", spelled.to_str().unwrap()).contains(&format!(":{colon_env}:")),
        "PATH spells the directory across two entries"
    );
    for path in [&installer.with_minisign, &spelled] {
        for shell in shells {
            let install = installer.run(path, &[("SHELL", shell), ("XDG_BIN_HOME", colon_env)]);
            assert!(install.status.success(), "{}", err(&install));
            let lines = ending(&install);
            assert_eq!(lines.len(), 6, "{shell}: {lines:#?}");
            assert_eq!(lines[5], refusal, "{shell}: {lines:#?}");
            no_command(&lines);
        }
    }

    // A newline, a tab, an ESC and a C1 CSI: the directory is never printed, so no raw control
    // reaches the terminal and the ending keeps its lines.
    let refusal = "  no PATH entry can hold the bin directory, which has a control character: set \
                   XDG_BIN_HOME to a directory without one, then install again";
    for control in ["\n", "\t", "\u{1b}[31m", "\u{9b}31m"] {
        let dir = sandbox.path(&format!("af{control}bin"));
        let dir_env = dir.to_str().unwrap();
        for shell in shells {
            let install = installer.run(
                &installer.with_minisign,
                &[("SHELL", shell), ("XDG_BIN_HOME", dir_env)],
            );
            assert!(install.status.success(), "{}", err(&install));
            let lines = ending(&install);
            assert_eq!(lines.len(), 6, "{control:?} {shell}: {lines:#?}");
            assert_eq!(lines[5], refusal, "{control:?} {shell}: {lines:#?}");
            no_command(&lines);
            assert!(
                !out(&install).chars().any(|c| c.is_control() && c != '\n'),
                "{control:?} {shell}: {:?}",
                out(&install)
            );
        }
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
