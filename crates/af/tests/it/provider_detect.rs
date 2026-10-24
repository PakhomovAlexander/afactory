//! `af provider detect`, and the `status`, `add` and `setup` fixes that came with it (ADR-0197).
//!
//! Every machine here is a private home with fake `claude` and `codex` scripts that answer af's
//! capability check and the status probe the way the official CLIs do, keyed on a `logged-in`
//! file in the directory they are pointed at. The fake Codex also leaves a `probed` file behind,
//! as the real one creates its home on first contact, so a test can tell which directories were
//! handed to it. No test reads a credential, starts a login or spends a token.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

use crate::provider_cli::{CLAUDE_HELP, with_help, with_page};

/// A Claude CLI: logged in where `$CLAUDE_CONFIG_DIR/logged-in` exists, and then reporting the
/// account identity no af document may repeat.
const CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' '2.1.299 (Claude Code)'
  exit 0
fi
if [ "$1" = auth ] && [ "$2" = status ] && [ "$3" = --json ]; then
  if [ -f "$CLAUDE_CONFIG_DIR/logged-in" ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"person@example.test","orgName":"Secret Org","subscriptionType":"max"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'
  exit 1
fi
exit 64
"#;

/// A Codex CLI that leaves `probed` in every directory its login status is asked about.
const CODEX: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.163.0'
  exit 0
fi
if [ "$1" = login ] && [ "$2" = status ]; then
  : > "$CODEX_HOME/probed"
  if [ -f "$CODEX_HOME/logged-in" ]; then
    printf '%s\n' 'Logged in using ChatGPT' >&2
    exit 0
  fi
  printf '%s\n' 'Not logged in' >&2
  exit 1
fi
exit 64
"#;

struct Machine {
    _temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl Machine {
    /// A private home, with an empty `~/bin` as the whole `PATH`.
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let bin = home.join("bin");
        for dir in [&home, &bin] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            _temp: temp,
            root,
            home,
            bin,
        }
    }

    /// `~/relative`, private, holding an empty file per name.
    fn dir(&self, relative: &str, files: &[&str]) -> PathBuf {
        let dir = self.home.join(relative);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        for file in files {
            std::fs::write(dir.join(file), "").unwrap();
        }
        dir
    }

    /// A fake CLI in `~/bin`, answering af's capability check.
    fn install(&self, name: &str, script: &str) -> PathBuf {
        self.install_in(&self.bin, name, &with_help(name, script))
    }

    fn install_in(&self, dir: &Path, name: &str, script: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Make a system tool reachable from the fixture `PATH`, for a script that needs one.
    fn link(&self, tool: &str) {
        let target = ["/bin", "/usr/bin"]
            .iter()
            .map(|dir| Path::new(dir).join(tool))
            .find(|path| path.exists())
            .unwrap();
        std::os::unix::fs::symlink(target, self.bin.join(tool)).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = crate::common::af();
        command
            .args(args)
            // Outside any checkout, so no repository's af pin is consulted.
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("PATH", &self.bin)
            .env("SHELL", "/bin/zsh")
            .env("NO_COLOR", "1")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME");
        command
    }

    fn af(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn registry(&self) -> String {
        std::fs::read_to_string(self.home.join(".config/af/providers.toml")).unwrap_or_default()
    }

    fn registered(&self) -> Vec<String> {
        let registry = self.registry();
        if registry.is_empty() {
            return Vec::new();
        }
        let value: toml::Table = registry.parse().unwrap();
        value["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// The item-3 machine: `~/.config/claude-work` and `~/.codex-acme` logged in, and
    /// `~/.config/claude-personal` logged in and named by `CLAUDE_CONFIG_DIR`.
    fn logins(&self) -> PathBuf {
        self.install("claude", CLAUDE);
        self.install("codex", CODEX);
        self.dir(".config/claude-work", &[".claude.json", "logged-in"]);
        self.dir(".codex-acme", &["auth.json", "logged-in"]);
        self.dir(".config/claude-personal", &[".claude.json", "logged-in"])
    }
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

/// stdout as the one `af/provider-detect@3` document, valid against the published schema.
fn detected(output: &Output) -> Value {
    let stdout = out(output);
    let document: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout is not one document: {error}\n{stdout}"));
    crate::schemas::valid(
        &crate::schemas::validator("provider-detect-v3.json"),
        &document,
    );
    assert_eq!(document["exit_code"], code(output), "{document}");
    document
}

fn candidate<'a>(document: &'a Value, dir: &Path) -> &'a Value {
    let dir = dir.to_str().unwrap();
    let rows: Vec<&Value> = document["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["auth_dir"] == dir)
        .collect();
    assert_eq!(rows.len(), 1, "{dir} in {document}");
    rows[0]
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Item 1 and 7: with nothing installed and nothing found, detect is the install guidance, not
/// a dead end, and exits 0.
#[test]
fn detect_with_nothing_found_prints_the_install_guidance() {
    let machine = Machine::new();
    let output = machine.af(&["provider", "detect"]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    let stdout = out(&output);
    for needle in [
        "No supported Provider CLI was found",
        "af help providers",
        "  claude  npm install -g @anthropic-ai/claude-code",
        "          or: curl -fsSL https://claude.ai/install.sh | bash",
        "  codex   npm install -g @openai/codex",
        "          or: brew install codex",
        "Searched the directories on PATH:\n  ~/bin\n",
        "and the usual install directories:\n  ~/.local/bin\n  ~/.claude/local\n  ~/.npm-global/bin\n  ~/.volta/bin\n  ~/.bun/bin\n",
        "then run: af provider detect",
    ] {
        assert!(stdout.contains(needle), "`{needle}` missing:\n{stdout}");
    }
    assert!(
        stdout.lines().all(|line| line.chars().count() < 100),
        "{stdout}"
    );

    let json = machine.af(&["provider", "detect", "--json"]);
    assert_eq!(code(&json), 0, "{}", err(&json));
    let document = detected(&json);
    assert_eq!(document["candidates"], serde_json::json!([]));
    assert_eq!(document["binaries"], serde_json::json!([]));
    // GA final fix item 6: the next actions are typed fields, not only prose.
    assert_eq!(
        document["install"],
        serde_json::json!([
            {
                "kind": "claude",
                "command": "npm install -g @anthropic-ai/claude-code",
                "docs": "https://code.claude.com/docs/en/setup",
            },
            {
                "kind": "codex",
                "command": "npm install -g @openai/codex",
                "docs": "https://developers.openai.com/codex/cli",
            },
        ]),
        "{document}"
    );
    assert_eq!(document["next_action"], "af provider detect", "{document}");
    assert!(
        document["notes"][0]
            .as_str()
            .unwrap()
            .contains("npm install -g @anthropic-ai/claude-code"),
        "{document}"
    );
    // Detection writes nothing, not even a registry.
    assert!(machine.registry().is_empty());
}

/// Item 7: `af provider status` with no CLI and no registry entry prints the same guidance, and
/// with directories present but unregistered names them, unprobed.
#[test]
fn status_without_a_cli_or_a_registry_prints_the_guidance_and_names_what_it_found() {
    let machine = Machine::new();
    let empty = machine.af(&["provider", "status"]);
    assert_eq!(code(&empty), 0, "{}", err(&empty));
    let stdout = out(&empty);
    assert!(
        !stdout.contains("No supported provider CLI is installed"),
        "{stdout}"
    );
    for needle in [
        "No supported Provider CLI was found",
        "npm install -g @anthropic-ai/claude-code",
        "brew install codex",
        "then run: af provider detect",
    ] {
        assert!(stdout.contains(needle), "`{needle}` missing:\n{stdout}");
    }
    assert!(!stdout.contains("found but not registered"), "{stdout}");

    let work = machine.dir(".config/claude-work", &[".claude.json"]);
    let acme = machine.dir(".codex-acme", &["auth.json"]);
    let found = machine.af(&["provider", "status"]);
    assert_eq!(code(&found), 0, "{}", err(&found));
    let stdout = out(&found);
    assert!(stdout.contains("then run: af provider detect"), "{stdout}");
    assert!(
        stdout.contains(
            "found but not registered: ~/.config/claude-work, ~/.codex-acme (not probed) \u{2192} af provider detect"
        ),
        "{stdout}"
    );
    // A names-only scan: nothing below either directory was created.
    assert_eq!(entries(&work), [".claude.json"]);
    assert_eq!(entries(&acme), ["auth.json"]);

    // With the CLI installed the default directory is a row of its own, and the line names
    // only what no row lists.
    machine.install("claude", CLAUDE);
    machine.dir(".claude", &["settings.json"]);
    let ambient = machine.af(&["provider", "status"]);
    assert_eq!(code(&ambient), 0, "{}", err(&ambient));
    let stdout = out(&ambient);
    assert!(stdout.contains("claude-ambient "), "{stdout}");
    assert!(
        stdout.contains(
            "found but not registered: ~/.config/claude-work, ~/.codex-acme (not probed) \u{2192} af provider detect"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("No supported Provider CLI"), "{stdout}");
}

/// Item 2: a CLI only in `~/.local/bin`, off `PATH`, is found there, reported with the PATH fix
/// for the operator's shell, and probed through that path.
#[test]
fn an_off_path_cli_is_found_with_its_path_fix_and_probed_through_that_path() {
    let machine = Machine::new();
    let local = machine.home.join(".local/bin");
    machine.install_in(&local, "claude", &with_help("claude", CLAUDE));
    let work = machine.dir(".config/claude-work", &[".claude.json", "logged-in"]);

    let output = machine.af(&["provider", "detect"]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    let stdout = out(&output);
    assert!(
        stdout.contains("  claude found off PATH: ~/.local/bin/claude\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("    fix: export PATH=\"$HOME/.local/bin:$PATH\"\n"),
        "{stdout}"
    );
    let row = stdout
        .lines()
        .find(|line| line.contains("~/.config/claude-work"))
        .unwrap_or_else(|| panic!("no claude-work row:\n{stdout}"));
    assert!(row.contains("Claude Max (claude.ai)"), "{stdout}");

    let fish = machine
        .command(&["provider", "detect", "--json"])
        .env("SHELL", "/usr/local/bin/fish")
        .output()
        .unwrap();
    let document = detected(&fish);
    assert_eq!(document["binaries"][0]["on_path"], false);
    assert_eq!(
        document["binaries"][0]["path"],
        local.join("claude").to_str().unwrap()
    );
    assert_eq!(candidate(&document, &work)["auth"], "authenticated");
    let notes = document["notes"].to_string();
    assert!(notes.contains("fish_add_path ~/.local/bin"), "{notes}");
}

/// Item 3: every source, once per directory, the selector variable's directory not repeated as
/// the sibling name it also matches.
#[test]
fn candidates_carry_their_source_once_each() {
    let machine = Machine::new();
    let personal = machine.logins();
    let work = machine.home.join(".config/claude-work");
    let acme = machine.home.join(".codex-acme");
    let scan = |extra: &[&str]| {
        let mut args = vec!["provider", "detect", "--json"];
        args.extend_from_slice(extra);
        let output = machine
            .command(&args)
            .env("CLAUDE_CONFIG_DIR", &personal)
            .output()
            .unwrap();
        assert_eq!(code(&output), 0, "{}", err(&output));
        detected(&output)
    };
    let document = scan(&[]);
    let rows: Vec<(String, String, String)> = document["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["kind"].as_str().unwrap().to_string(),
                row["auth_dir"].as_str().unwrap().to_string(),
                row["source"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let row = |kind: &str, dir: &Path, source: &str| {
        (
            kind.to_string(),
            dir.to_str().unwrap().to_string(),
            source.to_string(),
        )
    };
    assert_eq!(
        rows,
        [
            row("claude", &personal, "env"),
            row("claude", &work, "sibling"),
            row("codex", &acme, "sibling"),
        ]
    );
    assert_eq!(
        candidate(&document, &personal)["markers"],
        serde_json::json!([".claude.json"])
    );

    // A registered directory outside the sibling names is a registry row; one inside keeps its
    // sibling source and says what it is registered as; --dir adds an explicit one.
    let team = machine.dir("teams/claude-team", &["settings.json", "logged-in"]);
    for (id, dir) in [("claude-work", &work), ("claude-team", &team)] {
        let added = machine.af(&[
            "provider",
            "add",
            id,
            "--kind",
            "claude",
            "--auth-dir",
            dir.to_str().unwrap(),
        ]);
        assert_eq!(code(&added), 0, "{}", err(&added));
    }
    let extra = machine.dir("teams/ops", &["config.toml", "logged-in"]);
    let document = scan(&["--dir", extra.to_str().unwrap()]);
    assert_eq!(candidate(&document, &work)["source"], "sibling");
    assert_eq!(candidate(&document, &work)["registered_as"], "claude-work");
    assert_eq!(candidate(&document, &team)["source"], "registry");
    assert_eq!(candidate(&document, &team)["registered_as"], "claude-team");
    let ops = candidate(&document, &extra);
    assert_eq!(ops["source"], "explicit");
    assert_eq!(ops["kind"], "codex");
    assert_eq!(ops["suggested"]["id"], "codex-ops");
}

/// Item 4: a directory without a marker is listed `empty`, nothing is created in it, and only
/// `--all` hands it to its CLI; an unsafe one shows `safe: no` with the reason.
#[test]
fn an_empty_directory_is_left_alone_unless_all_and_an_unsafe_one_says_why() {
    let machine = Machine::new();
    machine.install("codex", CODEX);
    let acme = machine.dir(".codex-acme", &["auth.json", "logged-in"]);
    let fresh = machine.dir(".codex-new", &[]);
    let shared = machine.dir(".codex-shared", &["auth.json", "logged-in"]);
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();

    let output = machine.af(&["provider", "detect"]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    let stdout = out(&output);
    let row = |dir: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with(&format!("codex  {dir} ")))
            .unwrap_or_else(|| panic!("no {dir} row:\n{stdout}"))
            .to_string()
    };
    assert!(
        row("~/.codex-new").contains("empty (not probed)"),
        "{stdout}"
    );
    assert!(
        row("~/.codex-shared").contains("unsafe (not probed)"),
        "{stdout}"
    );
    assert!(row("~/.codex-acme").contains("ChatGPT"), "{stdout}");
    assert!(
        stdout.contains("~/.codex-new: empty: no auth.json or config.toml"),
        "{stdout}"
    );
    let flat = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(&format!(
            "~/.codex-shared: safe: no: auth directory {} is writable by another user",
            shared.display()
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("\n  chmod go-w -- {}\n", shared.display())),
        "{stdout}"
    );
    assert!(entries(&fresh).is_empty(), "{:?}", entries(&fresh));
    assert!(!shared.join("probed").exists());
    assert!(acme.join("probed").exists());

    let json = machine.af(&["provider", "detect", "--json"]);
    let document = detected(&json);
    let empty = candidate(&document, &fresh);
    assert_eq!(empty["markers"], serde_json::json!([]));
    assert_eq!(empty["auth"], "not_probed");
    assert_eq!(empty["suggested"]["command"], "af provider detect --all");
    let unsafe_row = candidate(&document, &shared);
    assert_eq!(unsafe_row["safe"], false);
    assert_eq!(unsafe_row["auth"], "not_probed");
    assert_eq!(unsafe_row["suggested"]["action"], "fix_safety");
    assert!(entries(&fresh).is_empty(), "{:?}", entries(&fresh));

    let all = machine.af(&["provider", "detect", "--all", "--json"]);
    let document = detected(&all);
    assert_eq!(candidate(&document, &fresh)["auth"], "not_authenticated");
    assert_eq!(entries(&fresh), ["probed"]);
    // `--all` never probes an unsafe directory.
    assert!(!shared.join("probed").exists());
}

/// Item 5: the rows use the status probe's closed vocabulary, and a CLI that does not accept
/// af's invocation or cannot start is reported, rows still listed, under exit 4.
#[test]
fn detect_reports_the_status_probe_vocabulary_and_a_cli_that_cannot_serve_af() {
    let machine = Machine::new();
    machine.install("claude", CLAUDE);
    machine.install("codex", CODEX);
    let work = machine.dir(".config/claude-work", &[".claude.json", "logged-in"]);
    let default = machine.dir(".claude", &["settings.json"]);
    let acme = machine.dir(".codex-acme", &["config.toml", "logged-in"]);
    let output = machine.af(&["provider", "detect", "--json"]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    let document = detected(&output);
    let work_row = candidate(&document, &work);
    assert_eq!(work_row["auth"], "authenticated");
    assert_eq!(work_row["auth_method"], "oauth");
    assert_eq!(work_row["plan"], "max");
    assert_eq!(candidate(&document, &default)["auth"], "not_authenticated");
    assert_eq!(candidate(&document, &default)["auth_method"], "none");
    let codex = candidate(&document, &acme);
    assert_eq!(codex["auth"], "authenticated");
    assert_eq!(codex["plan"], "unknown");
    for binary in document["binaries"].as_array().unwrap() {
        assert_eq!(binary["compatible"], true, "{binary}");
    }
    assert_eq!(document["binaries"][0]["version"], "2.1.299");
    assert_eq!(document["binaries"][1]["version"], "0.163.0");

    // A Claude CLI without af's isolation flags.
    let old: String = CLAUDE_HELP
        .lines()
        .filter(|line| !line.contains("--safe-mode"))
        .map(|line| format!("{line}\n"))
        .collect();
    machine.install_in(&machine.bin, "claude", &with_page("claude", CLAUDE, &old));
    let output = machine.af(&["provider", "detect", "--json"]);
    assert_eq!(code(&output), 4, "{}", err(&output));
    let document = detected(&output);
    assert_eq!(document["binaries"][0]["compatible"], false);
    assert_eq!(candidate(&document, &work)["auth"], "incompatible");
    assert_eq!(
        candidate(&document, &work)["suggested"]["action"],
        "fix_cli"
    );
    assert_eq!(candidate(&document, &acme)["auth"], "authenticated");
    assert!(
        err(&output).contains("incompatible Provider CLI"),
        "{}",
        err(&output)
    );
    assert!(err(&output).contains("--safe-mode"), "{}", err(&output));
    let human = machine.af(&["provider", "detect"]);
    assert_eq!(code(&human), 4);
    assert!(out(&human).contains("CLI incompatible"), "{}", out(&human));
    // The rows carry the cause, and an --apply that exits 4 ends with the upgrade, not status.
    assert_eq!(candidate(&document, &work)["auth_cause"], "incompatible");
    let applied = machine.af(&["provider", "detect", "--apply"]);
    assert_eq!(code(&applied), 4, "{}", err(&applied));
    assert!(
        out(&applied).contains("\nnext: npm install -g @anthropic-ai/claude-code@latest\n"),
        "{}",
        out(&applied)
    );
    assert!(!out(&applied).contains("next: af provider status"));

    // A Claude CLI that cannot start at all.
    machine.install_in(
        &machine.bin,
        "claude",
        "#!/bin/sh\nprintf '%s\\n' 'Error: Cannot find module claude' >&2\nexit 1\n",
    );
    let output = machine.af(&["provider", "detect", "--json"]);
    assert_eq!(code(&output), 4, "{}", err(&output));
    let document = detected(&output);
    assert_eq!(candidate(&document, &work)["auth"], "installation_failed");
    assert_eq!(
        candidate(&document, &default)["auth"],
        "installation_failed"
    );
}

/// Item 6: an unsupported harness CLI on `PATH` is named with its version, never probed, and
/// none of its directories is scanned.
#[test]
fn unsupported_harness_clis_are_named_and_left_alone() {
    let machine = Machine::new();
    machine.install_in(
        &machine.bin,
        "gemini",
        r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' '0.9.0'
  exit 0
fi
: > "$HOME/gemini-ran"
exit 0
"#,
    );
    let gemini = machine.dir(".gemini", &["settings.json"]);
    let output = machine.af(&["provider", "detect"]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    assert!(
        out(&output).contains("gemini 0.9.0 (~/bin/gemini): not supported by af yet"),
        "{}",
        out(&output)
    );
    let json = machine.af(&["provider", "detect", "--json"]);
    let document = detected(&json);
    assert_eq!(
        document["unsupported"],
        serde_json::json!([{
            "name": "gemini",
            "path": machine.bin.join("gemini").to_str().unwrap(),
            "version": "0.9.0",
        }])
    );
    assert!(!document.to_string().contains(".gemini"), "{document}");
    assert!(!machine.home.join("gemini-ran").exists());
    assert_eq!(entries(&gemini), ["settings.json"]);
}

/// Item 5, C3 items 12-13: a CLI that hangs on its status probe is cut at detect's 10-second
/// bound whatever its kind (Claude's structural timeout is 30 s, Codex's 15 s), and the row says
/// `unavailable` with the cause, in the table and as `auth_cause` in the document.
#[test]
fn a_hanging_cli_is_cut_at_the_probe_timeout() {
    let machine = Machine::new();
    machine.link("sleep");
    machine.install(
        "codex",
        "#!/bin/sh\nif [ \"$1\" = --version ]; then\n  printf '%s\\n' 'codex-cli 0.163.0'\n  exit 0\nfi\nif [ \"$1\" = login ]; then\n  exec sleep 600\nfi\nexit 64\n",
    );
    machine.install(
        "claude",
        "#!/bin/sh\nif [ \"$1\" = --version ]; then\n  printf '%s\\n' '2.1.299 (Claude Code)'\n  exit 0\nfi\nif [ \"$1\" = auth ]; then\n  exec sleep 600\nfi\nexit 64\n",
    );
    let acme = machine.dir(".codex-acme", &["auth.json"]);
    let work = machine.dir(".config/claude-work", &[".claude.json"]);
    let started = std::time::Instant::now();
    let output = machine
        .command(&["provider", "detect"])
        .env_remove(crate::common::PROBE_TIMEOUT_SETTING)
        .output()
        .unwrap();
    assert_eq!(code(&output), 0, "{}", err(&output));
    // Both probes run at once and each ends at 10 s; the margin is for a loaded machine's
    // process start-up (ADR-0114), well below Claude's 30 s.
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(25), "{elapsed:?}");
    let stdout = out(&output);
    for (prefix, dir) in [
        ("codex  ~/.codex-acme ", "~/.codex-acme"),
        ("claude ~/.config/claude-work", "~/.config/claude-work"),
    ] {
        let row = stdout
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {dir} row:\n{stdout}"));
        assert!(row.contains("unavailable"), "{stdout}");
        assert!(
            stdout.contains(&format!(
                "{dir}: unavailable: provider status probe timed out after 10 seconds"
            )),
            "{stdout}"
        );
    }
    assert!(acme.exists());

    let json = machine
        .command(&["provider", "detect", "--json"])
        .env_remove(crate::common::PROBE_TIMEOUT_SETTING)
        .output()
        .unwrap();
    let document = detected(&json);
    for dir in [&acme, &work] {
        let row = candidate(&document, dir);
        assert_eq!(row["auth"], "unavailable", "{row}");
        assert_eq!(row["auth_cause"], "timeout", "{row}");
        assert_eq!(
            row["auth_detail"],
            "provider status probe timed out after 10 seconds"
        );
    }
}

#[test]
fn the_table_for_logins_in_three_directories() {
    let machine = Machine::new();
    let personal = machine.logins();
    let output = machine
        .command(&["provider", "detect"])
        .env("CLAUDE_CONFIG_DIR", &personal)
        .output()
        .unwrap();
    assert_eq!(code(&output), 0, "{}", err(&output));
    let stdout = out(&output);
    assert_eq!(
        stdout,
        "\
KIND   AUTH DIR                   SOURCE   LOGIN                  REGISTERED AS    SUGGESTED
claude ~/.config/claude-personal  env      Claude Max (claude.ai) -                claude-personal
claude ~/.config/claude-work      sibling  Claude Max (claude.ai) -                claude-work
codex  ~/.codex-acme              sibling  ChatGPT                -                codex-acme

binaries: claude 2.1.299, codex 0.163.0
  claude ~/bin/claude
  codex ~/bin/codex

next, one line per directory:
  af provider add claude-personal --kind claude --auth-dir ~/.config/claude-personal
  af provider add claude-work --kind claude --auth-dir ~/.config/claude-work
  af provider add codex-acme --kind codex --auth-dir ~/.codex-acme

3 logins are not registered: run af provider detect --apply to register them with the IDs above, or
  copy a line
shell aliases or functions that set CLAUDE_CONFIG_DIR/CODEX_HOME are not read; pass --dir for such
  a directory
"
    );
    assert!(
        stdout.lines().all(|line| line.chars().count() < 100),
        "{stdout}"
    );
    // The account the CLI reported is never printed without `--identity` at a terminal.
    assert!(!stdout.contains("person@example.test") && !stdout.contains("Secret Org"));
    assert!(
        machine.registry().is_empty(),
        "the dry run wrote the registry"
    );
}

/// Item 6: `--identity` adds the account label only at a terminal and never under `--json`.
#[test]
fn identity_labels_need_a_terminal_and_never_reach_json() {
    let machine = Machine::new();
    let personal = machine.logins();
    for args in [
        vec!["provider", "detect", "--identity"],
        vec!["provider", "detect", "--identity", "--json"],
    ] {
        let output = machine
            .command(&args)
            .env("CLAUDE_CONFIG_DIR", &personal)
            .output()
            .unwrap();
        assert_eq!(code(&output), 0, "{}", err(&output));
        let stdout = out(&output);
        assert!(!stdout.contains("person@example.test"), "{stdout}");
        assert!(!stdout.contains("Secret Org"), "{stdout}");
        assert!(!stdout.contains("account "), "{stdout}");
        assert!(
            err(&output).contains("--identity shows account labels only when stdout is a terminal"),
            "{}",
            err(&output)
        );
    }

    let (status, terminal) =
        in_terminal(&machine, &personal, &["provider", "detect", "--identity"]);
    assert_eq!(status, 0, "{terminal}");
    let labels = terminal
        .lines()
        .filter(|line| line.contains("account "))
        .count();
    assert_eq!(
        labels, 2,
        "one label per authenticated Claude row:\n{terminal}"
    );
}

/// `af` with all three standard streams on a pseudo-terminal, as an operator runs it.
fn in_terminal(machine: &Machine, personal: &Path, args: &[&str]) -> (u32, String) {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read as _;

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(crate::common::af_binary());
    command.args(args);
    command.env_clear();
    command.cwd(&machine.root);
    let probe = crate::common::LOAD_SAFE_PROBE_TIMEOUT_MS;
    for (name, value) in [
        ("HOME", machine.home.as_os_str()),
        ("PATH", machine.bin.as_os_str()),
        ("CLAUDE_CONFIG_DIR", personal.as_os_str()),
        ("SHELL", "/bin/zsh".as_ref()),
        ("AF_SELF_OFFLINE", "1".as_ref()),
        (crate::common::DETECT_SYSTEM_DIRS_SETTING, "".as_ref()),
        (crate::common::PROBE_TIMEOUT_SETTING, probe.as_ref()),
    ] {
        command.env(name, value);
    }
    command.env("XDG_CONFIG_HOME", machine.home.join(".config"));
    command.env("XDG_STATE_HOME", machine.home.join(".local/state"));
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let drained = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    let status = child.wait().unwrap();
    drop(pair.master);
    let text = String::from_utf8_lossy(&drained.join().unwrap()).into_owned();
    (status.exit_code(), text)
}

/// Item 8: `--apply` registers every logged-in, unregistered directory through `add`'s
/// publication, prints each line it ran, leaves the logged-out default directory alone with its
/// setup line, and a second `--apply` changes nothing.
#[test]
fn apply_registers_what_is_logged_in_and_a_second_apply_changes_nothing() {
    let machine = Machine::new();
    let personal = machine.logins();
    let default = machine.dir(".claude", &["settings.json"]);
    let apply = |args: &[&str]| {
        machine
            .command(args)
            .env("CLAUDE_CONFIG_DIR", &personal)
            .output()
            .unwrap()
    };
    let output = apply(&["provider", "detect", "--apply"]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    let stdout = out(&output);
    for needle in [
        "registered: af provider add claude-personal --kind claude --auth-dir ~/.config/claude-personal\n",
        "registered: af provider add claude-work --kind claude --auth-dir ~/.config/claude-work\n",
        "registered: af provider add codex-acme --kind codex --auth-dir ~/.codex-acme\n",
        "not logged in, so not registered: ~/.claude; run this yourself at a private terminal:\n  af provider setup claude-main --kind claude --auth-dir ~/.claude --login\n",
        "next: af provider status\n",
    ] {
        assert!(stdout.contains(needle), "`{needle}` missing:\n{stdout}");
    }
    assert_eq!(
        machine.registered(),
        ["claude-personal", "claude-work", "codex-acme"]
    );
    assert_eq!(entries(&default), ["settings.json"]);
    let registry = machine.registry();
    assert!(
        registry.contains(&format!(
            "auth_dir = \"{}\"",
            machine.home.join(".config/claude-work").display()
        )),
        "{registry}"
    );

    let again = apply(&["provider", "detect", "--apply"]);
    assert_eq!(code(&again), 0, "{}", err(&again));
    let stdout = out(&again);
    assert!(
        stdout.contains("already registered: claude-work (~/.config/claude-work)"),
        "{stdout}"
    );
    assert!(!stdout.contains("registered: af provider add"), "{stdout}");
    assert_eq!(
        machine.registry(),
        registry,
        "a second --apply rewrote the registry"
    );

    let json = apply(&["provider", "detect", "--apply", "--json"]);
    let document = detected(&json);
    assert_eq!(document["applied"], true);
    assert_eq!(
        candidate(&document, &personal)["applied"],
        serde_json::json!({
            "id": "claude-personal",
            "result": "already_registered",
            "conflict_with": null,
        })
    );
    assert_eq!(
        candidate(&document, &default)["applied"],
        serde_json::json!({
            "id": "claude-main",
            "result": "skipped_not_authenticated",
            "conflict_with": null,
        })
    );
    assert_eq!(machine.registry(), registry);
}

/// Item 8: a registry conflict stops `--apply` at that row with exit 5; the rows before it stay
/// published and the rows after it are not applied.
#[test]
fn a_registry_conflict_stops_apply_with_the_rows_before_it_published() {
    let machine = Machine::new();
    machine.install("claude", CLAUDE);
    machine.install("codex", CODEX);
    machine.dir(".config/claude-work", &[".claude.json", "logged-in"]);
    let acme = machine.dir(".codex-acme", &["auth.json", "logged-in"]);
    let taken = machine.dir("teams/claude-a", &[".claude.json", "logged-in"]);
    let other = machine.dir("teams/claude-b", &[".claude.json", "logged-in"]);
    let added = machine.af(&[
        "provider",
        "add",
        "claude-taken",
        "--kind",
        "claude",
        "--auth-dir",
        taken.to_str().unwrap(),
    ]);
    assert_eq!(code(&added), 0, "{}", err(&added));

    let output = machine.af(&[
        "provider",
        "detect",
        "--dir",
        other.to_str().unwrap(),
        "--id",
        "claude-taken",
        "--apply",
        "--json",
    ]);
    assert_eq!(code(&output), 5, "{}", err(&output));
    let document = detected(&output);
    let row = candidate(&document, &other);
    assert_eq!(
        row["applied"],
        serde_json::json!({
            "id": "claude-taken",
            "result": "registry_conflict",
            "conflict_with": "claude-taken",
        })
    );
    // The row suggests the retry that works, never the add that just failed.
    let retry = format!(
        "af provider detect --dir {} --id claude-taken-2 --apply",
        other.display()
    );
    assert_eq!(
        row["suggested"],
        serde_json::json!({"action": "retry_apply", "id": "claude-taken-2", "command": retry})
    );
    assert_eq!(candidate(&document, &acme)["applied"], Value::Null);
    assert_eq!(machine.registered(), ["claude-taken", "claude-work"]);
    assert!(
        err(&output).contains("registry conflict"),
        "{}",
        err(&output)
    );

    let human = machine.af(&[
        "provider",
        "detect",
        "--dir",
        other.to_str().unwrap(),
        "--id",
        "claude-taken",
        "--apply",
    ]);
    assert_eq!(code(&human), 5, "{}", err(&human));
    let stdout = out(&human).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        stdout.contains("already registered: claude-work"),
        "{stdout}"
    );
    assert!(
        stdout.contains("registry conflict at af provider add claude-taken"),
        "{stdout}"
    );
    assert!(
        stdout.contains("1 later login was not registered"),
        "{stdout}"
    );
    // Text and JSON agree: the message and the closing line name the same retry.
    let retry = "af provider detect --dir ~/teams/claude-b --id claude-taken-2 --apply";
    assert!(
        stdout.contains(&format!("choose another ID: {retry}")),
        "{stdout}"
    );
    assert!(
        out(&human).contains(&format!("\nnext: {retry}\n")),
        "{}",
        out(&human)
    );
    assert!(!out(&human).contains("next: af provider status"));
    assert_eq!(machine.registered(), ["claude-taken", "claude-work"]);

    // The retry runs as printed.
    let retried = machine.af(&[
        "provider",
        "detect",
        "--dir",
        other.to_str().unwrap(),
        "--id",
        "claude-taken-2",
        "--apply",
    ]);
    assert_eq!(code(&retried), 0, "{}", err(&retried));
    assert!(out(&retried).contains("\nnext: af provider status\n"));
    assert!(machine.registered().contains(&"claude-taken-2".to_string()));
}

/// Item 9: exit codes 0, 2, 4 and 5 only, the usage errors under `--json` as one error document.
#[test]
fn exit_codes_follow_the_documented_table() {
    let machine = Machine::new();
    machine.install("claude", CLAUDE);
    let a = machine.dir("teams/a", &[".claude.json"]);
    let b = machine.dir("teams/b", &[".claude.json"]);
    let bare = machine.dir("teams/bare", &[]);
    let both = machine.dir("teams/both", &[".claude.json", "auth.json"]);
    let (a, b, bare, both) = (
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        bare.to_str().unwrap(),
        both.to_str().unwrap(),
    );
    let cases: Vec<(Vec<&str>, i32)> = vec![
        (vec!["provider", "detect"], 0),
        (vec!["provider", "detect", "--dir", a], 0),
        (
            vec!["provider", "detect", "--dir", a, "--id", "claude-a"],
            0,
        ),
        (
            vec!["provider", "detect", "--dir", bare, "--kind", "codex"],
            0,
        ),
        (vec!["provider", "detect", "--id", "claude-a"], 2),
        (vec!["provider", "detect", "--kind", "gemini"], 2),
        (vec!["provider", "detect", "--dir", a, "--dir", b], 2),
        (
            vec!["provider", "detect", "--dir", a, "--dir", b, "--id", "x"],
            2,
        ),
        (vec!["provider", "detect", "--dir", bare], 2),
        (vec!["provider", "detect", "--dir", both], 2),
        (
            vec!["provider", "detect", "--dir", a, "--id", "claude-ambient"],
            2,
        ),
    ];
    for (args, expected) in cases {
        let output = machine.af(&args);
        assert_eq!(code(&output), expected, "{args:?}: {}", err(&output));
        assert!([0, 1, 2, 4, 5].contains(&code(&output)));
        if expected == 2 && args.contains(&"--dir") {
            let mut json = args.clone();
            json.push("--json");
            let output = machine.af(&json);
            assert_eq!(code(&output), 2, "{json:?}");
            let document: Value = serde_json::from_str(out(&output).trim()).unwrap();
            assert_eq!(document["schema"], "af/error@1", "{document}");
            assert_eq!(document["exit_code"], 2);
            assert!(err(&output).contains("try: af provider detect --help"));
        }
    }
    let message = err(&machine.af(&["provider", "detect", "--dir", a, "--dir", b]));
    assert!(message.contains("pass one --id per --dir"), "{message}");
    let message = err(&machine.af(&["provider", "detect", "--dir", bare]));
    assert!(
        message.contains(&format!("--dir {bare} holds no login marker")),
        "{message}"
    );
    assert!(
        message.contains("pass --kind claude or --kind codex"),
        "{message}"
    );

    // 4: a found CLI that cannot serve af; the rows are still reported.
    machine.install_in(
        &machine.bin,
        "claude",
        "#!/bin/sh\nprintf '%s\\n' 'Error: broken' >&2\nexit 1\n",
    );
    let output = machine.af(&["provider", "detect", "--dir", a]);
    assert_eq!(code(&output), 4, "{}", err(&output));
    assert!(
        out(&output).contains("CLI cannot start"),
        "{}",
        out(&output)
    );
}

/// Item 10: `af provider add` asks the CLI first and refuses a context with no login, naming the
/// directory and the setup line, unless `--unverified`, which says so.
#[test]
fn add_refuses_a_context_that_is_not_logged_in_unless_unverified() {
    let machine = Machine::new();
    machine.install("claude", CLAUDE);
    let work = machine.dir("profiles/claude-work", &[".claude.json"]);
    let path = work.to_str().unwrap();
    let args = [
        "provider",
        "add",
        "claude-work",
        "--kind",
        "claude",
        "--auth-dir",
        path,
    ];
    let refused = machine.af(&args);
    assert_eq!(code(&refused), 6, "{}", err(&refused));
    let stderr = err(&refused);
    assert!(
        stderr.contains(&format!("auth directory {path} has no claude login")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "af provider setup claude-work --kind claude --auth-dir {path} --login"
        )),
        "{stderr}"
    );
    assert!(stderr.contains("--unverified"), "{stderr}");
    assert!(machine.registry().is_empty());

    let mut unverified = args.to_vec();
    unverified.push("--unverified");
    let added = machine.af(&unverified);
    assert_eq!(code(&added), 0, "{}", err(&added));
    assert!(
        err(&added).contains("warning: not verified here"),
        "{}",
        err(&added)
    );
    assert_eq!(machine.registered(), ["claude-work"]);

    // A logged-in context is registered without a warning.
    let team = machine.dir("profiles/claude-team", &[".claude.json", "logged-in"]);
    let added = machine.af(&[
        "provider",
        "add",
        "claude-team",
        "--kind",
        "claude",
        "--auth-dir",
        team.to_str().unwrap(),
    ]);
    assert_eq!(code(&added), 0, "{}", err(&added));
    assert!(!err(&added).contains("warning"), "{}", err(&added));
}

/// Item 11: setup creates a missing auth directory only together with `--login`.
#[test]
fn setup_creates_a_missing_auth_directory_only_with_login() {
    let machine = Machine::new();
    machine.install("codex", CODEX);
    let fresh = machine.home.join("profiles/codex-new");
    let path = fresh.to_str().unwrap();
    let base = [
        "provider",
        "setup",
        "codex-new",
        "--kind",
        "codex",
        "--auth-dir",
        path,
    ];
    let refused = machine.af(&base);
    assert_eq!(code(&refused), 1, "{}", err(&refused));
    assert!(
        err(&refused).contains(&format!(
            "auth directory {path} does not exist; pass --login to create it, or check the path"
        )),
        "{}",
        err(&refused)
    );
    assert!(!fresh.exists());
    let mut json = base.to_vec();
    json.push("--json");
    let refused = machine.af(&json);
    assert_eq!(code(&refused), 1);
    let document: Value = serde_json::from_str(out(&refused).trim()).unwrap();
    assert_eq!(document["schema"], "af/error@1");
    assert!(!fresh.exists());

    // With `--login` the directory is created for the login, which this pipe never starts.
    let mut login = base.to_vec();
    login.push("--login");
    let output = machine.af(&login);
    assert_eq!(code(&output), 3, "{}", err(&output));
    assert!(fresh.is_dir());
    assert_eq!(
        std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

/// Item 12: one fix per status row, the CLI's own login command only under `--verbose`, and no
/// `--usage` hint when no row is authenticated.
#[test]
fn status_rows_name_one_setup_line_and_verbose_adds_the_cli_login() {
    let machine = Machine::new();
    machine.install("claude", CLAUDE);
    let work = machine.dir("profiles/claude-work", &[".claude.json"]);
    let path = work.to_str().unwrap();
    let added = machine.af(&[
        "provider",
        "add",
        "claude-work",
        "--kind",
        "claude",
        "--auth-dir",
        path,
        "--unverified",
    ]);
    assert_eq!(code(&added), 0, "{}", err(&added));
    let status = machine.af(&["provider", "status"]);
    assert_eq!(code(&status), 0, "{}", err(&status));
    assert_eq!(
        out(&status),
        format!(
            "\
ID                       KIND     STATUS              AUTH               SUBSCRIPTION
claude-work              claude   not authenticated   -                  -
  note   no Claude login in {path}; fix: af provider setup claude-work --kind claude --auth-dir {path} --login
"
        )
    );
    let verbose = machine.af(&["provider", "status", "--verbose"]);
    assert_eq!(code(&verbose), 0, "{}", err(&verbose));
    assert!(
        out(&verbose).ends_with(&format!(
            "  login  CLAUDE_CONFIG_DIR={path} claude auth login\n"
        )),
        "{}",
        out(&verbose)
    );
}

/// Item 13: status never hands a directory without a marker to its CLI, so a fresh home keeps
/// no `~/.codex`, and detect does not create one either.
#[test]
fn status_and_detect_leave_a_fresh_home_without_a_codex_directory() {
    let machine = Machine::new();
    machine.link("mkdir");
    // Like codex-cli: every invocation creates its home first.
    let body = with_help(
        "codex",
        "#!/bin/sh\n: > /dev/null\nif [ \"$1\" = login ] && [ \"$2\" = status ]; then\n  printf '%s\\n' 'Not logged in' >&2\n  exit 1\nfi\nexit 64\n",
    );
    let (interpreter, rest) = body.split_once('\n').unwrap();
    machine.install_in(
        &machine.bin,
        "codex",
        &format!("{interpreter}\nmkdir -p \"${{CODEX_HOME:-$HOME/.codex}}\"\n{rest}"),
    );
    let status = machine.af(&["provider", "status"]);
    assert_eq!(code(&status), 0, "{}", err(&status));
    let stdout = out(&status);
    let row = stdout
        .lines()
        .find(|line| line.starts_with("codex-ambient "))
        .unwrap_or_else(|| panic!("no codex-ambient row:\n{stdout}"));
    assert!(row.contains("not probed"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "{} does not exist, so af did not ask the codex CLI about it",
            machine.home.join(".codex").display()
        )),
        "{stdout}"
    );
    assert!(
        !machine.home.join(".codex").exists(),
        "status created ~/.codex"
    );
    let json = machine.af(&["provider", "status", "--json"]);
    assert_eq!(code(&json), 0, "{}", err(&json));
    let document: Value = serde_json::from_str(out(&json).trim()).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("provider-status-v3.json"),
        &document,
    );
    assert_eq!(document["providers"][0]["auth"], "not_probed");

    let detect = machine.af(&["provider", "detect"]);
    assert_eq!(code(&detect), 0, "{}", err(&detect));
    assert!(
        !machine.home.join(".codex").exists(),
        "detect created ~/.codex"
    );
}

/// Item 13: setup's auth-context lock lives under `$XDG_STATE_HOME/af/locks`, never in the auth
/// directory.
#[test]
fn setup_keeps_its_lock_in_the_state_directory() {
    let machine = Machine::new();
    machine.install("codex", CODEX);
    let auth = machine.dir("profiles/codex-main", &["auth.json", "logged-in"]);
    let output = machine.af(&[
        "provider",
        "setup",
        "codex-main",
        "--kind",
        "codex",
        "--auth-dir",
        auth.to_str().unwrap(),
    ]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    assert!(
        entries(&auth).iter().all(|name| !name.starts_with(".af-")),
        "{:?}",
        entries(&auth)
    );
    let lock = machine.home.join(format!(
        ".local/state/af/locks/codex-{}",
        crate::common::sha256_hex(auth.to_str().unwrap().as_bytes())
    ));
    let metadata = std::fs::metadata(&lock).unwrap_or_else(|error| panic!("{lock:?}: {error}"));
    assert!(metadata.is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(machine.registered(), ["codex-main"]);
}

/// C3 item 8: a directory that grants search but not read (mode 0100) cannot be listed, yet its
/// markers are checked by name, it is probed, and the row says its listing was unreadable; it is
/// never reported as an empty, unprobed directory.
#[test]
fn an_unlistable_directory_is_probed_by_its_marker_names() {
    let machine = Machine::new();
    machine.install("claude", CLAUDE);
    let work = machine.dir(".config/claude-work", &[".claude.json", "logged-in"]);
    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o100)).unwrap();
    let output = machine.af(&["provider", "detect", "--json"]);
    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o700)).unwrap();
    let document = detected(&output);
    let row = candidate(&document, &work);
    assert_eq!(row["listing_unreadable"], true, "{row}");
    assert_eq!(row["markers"], serde_json::json!([".claude.json"]), "{row}");
    assert_ne!(row["auth"], "not_probed", "{row}");
    assert_eq!(row["auth"], "authenticated", "{row}");

    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o100)).unwrap();
    let text = machine.af(&["provider", "detect"]);
    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o700)).unwrap();
    let stdout = out(&text);
    assert!(!stdout.contains("empty (not probed)"), "{stdout}");
    assert!(
        stdout.contains("~/.config/claude-work: listing unreadable"),
        "{stdout}"
    );
}

/// C3 item 10: a login whose CLI is only off `PATH` is not registered by `--apply`, since
/// neither status nor a Task could run it: exit 4, the PATH fix as the row's suggestion and the
/// closing line, and nothing in the registry.
#[test]
fn apply_leaves_a_login_whose_cli_is_off_path_unregistered_with_the_path_fix() {
    let machine = Machine::new();
    let local = machine.home.join(".local/bin");
    machine.install_in(&local, "claude", &with_help("claude", CLAUDE));
    let work = machine.dir(".config/claude-work", &[".claude.json", "logged-in"]);
    let fix = "export PATH=\"$HOME/.local/bin:$PATH\"";

    let output = machine.af(&["provider", "detect", "--apply", "--json"]);
    assert_eq!(code(&output), 4, "{}", err(&output));
    let document = detected(&output);
    let row = candidate(&document, &work);
    assert_eq!(row["suggested"]["action"], "fix_path", "{row}");
    assert_eq!(row["suggested"]["command"], fix, "{row}");
    assert_eq!(row["applied"]["result"], "skipped_cli_off_path", "{row}");
    assert!(machine.registered().is_empty());

    let text = machine.af(&["provider", "detect", "--apply"]);
    assert_eq!(code(&text), 4, "{}", err(&text));
    let stdout = out(&text);
    assert!(stdout.contains("its claude CLI is off PATH"), "{stdout}");
    assert!(stdout.contains(&format!("\nnext: {fix}\n")), "{stdout}");
    assert!(!stdout.contains("next: af provider status"), "{stdout}");
    assert!(machine.registered().is_empty());
}

/// C3 item 11: a registry that cannot be read is unknown, never empty: every row's registration
/// says so, the note is in every output (the empty scan's included), and `--apply` refuses with
/// exit 1 and one `af/error@1` document.
#[test]
fn an_unreadable_registry_is_unknown_and_refuses_apply() {
    let machine = Machine::new();
    let registry = machine.home.join(".config/af/providers.toml");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(&registry, "version = 1\n[[providers]\n").unwrap();

    // Nothing installed, nothing found: the note is still there.
    let empty = machine.af(&["provider", "detect", "--json"]);
    let document = detected(&empty);
    assert!(
        document["notes"]
            .to_string()
            .contains("the provider registry could not be read"),
        "{document}"
    );
    let text = out(&machine.af(&["provider", "detect"]));
    assert!(
        text.contains("note: the provider registry could not be read"),
        "{text}"
    );

    machine.install("claude", CLAUDE);
    let work = machine.dir(".config/claude-work", &[".claude.json", "logged-in"]);
    let scanned = machine.af(&["provider", "detect", "--json"]);
    assert_eq!(code(&scanned), 0, "{}", err(&scanned));
    let document = detected(&scanned);
    let row = candidate(&document, &work);
    assert_eq!(row["registration"], "unknown", "{row}");
    assert_eq!(row["registered_as"], Value::Null, "{row}");
    assert_eq!(row["suggested"]["action"], "fix_registry", "{row}");
    let table = out(&machine.af(&["provider", "detect"]));
    let line = table
        .lines()
        .find(|line| line.contains("~/.config/claude-work"))
        .unwrap();
    assert!(line.contains(" unknown "), "{table}");

    let refused = machine.af(&["provider", "detect", "--apply", "--json"]);
    assert_eq!(code(&refused), 1, "{}", err(&refused));
    let error: Value = serde_json::from_str(out(&refused).trim()).unwrap();
    assert_eq!(error["schema"], "af/error@1");
    assert_eq!(error["exit_code"], 1);
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("--apply registered nothing"),
        "{error}"
    );
    assert_eq!(
        std::fs::read_to_string(&registry).unwrap(),
        "version = 1\n[[providers]\n"
    );
}

/// C3 items 17-19: an empty scan says whether `--apply` was given; a Claude-only scan gives no
/// Codex advice; and with no private scratch directory a Claude CLI is checked with Claude's
/// capabilities, never labelled incompatible.
#[test]
fn small_scan_reports_say_what_was_asked() {
    let machine = Machine::new();
    for (args, applied) in [
        (vec!["provider", "detect", "--json"], false),
        (vec!["provider", "detect", "--apply", "--json"], true),
    ] {
        let output = machine.af(&args);
        assert_eq!(code(&output), 0, "{}", err(&output));
        assert_eq!(detected(&output)["applied"], applied, "{args:?}");
    }

    machine.install("claude", CLAUDE);
    let claude_only = machine.af(&["provider", "detect", "--kind", "claude"]);
    assert_eq!(code(&claude_only), 0, "{}", err(&claude_only));
    let stdout = out(&claude_only);
    assert!(!stdout.contains("codex"), "{stdout}");

    let unwritable = machine.root.join("no-such-tmp");
    let output = machine
        .command(&["provider", "detect", "--json"])
        .env("TMPDIR", &unwritable)
        .output()
        .unwrap();
    assert_ne!(code(&output), 4, "{}\n{}", out(&output), err(&output));
    let document = detected(&output);
    assert_ne!(document["binaries"][0]["compatible"], false, "{document}");
}

/// C3 items 16 and 28: one names-only scan lists the directories found but not registered, and
/// `af provider status` (text and `--json`), `af doctor` and `af onboard` list the same ones.
#[test]
fn status_doctor_and_onboarding_list_the_same_unregistered_directories() {
    let machine = Machine::new();
    let work = machine.dir(".config/claude-work", &[".claude.json"]);
    let acme = machine.dir(".codex-acme", &["auth.json"]);
    let shown = "~/.config/claude-work, ~/.codex-acme";

    let text = out(&machine.af(&["provider", "status"]));
    assert!(
        text.contains(&format!("found but not registered: {shown} (not probed)")),
        "{text}"
    );
    let status = machine.af(&["provider", "status", "--json"]);
    assert_eq!(code(&status), 0, "{}", err(&status));
    let document: Value = serde_json::from_str(out(&status).trim()).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("provider-status-v3.json"),
        &document,
    );
    assert_eq!(
        document["found_unregistered"],
        serde_json::json!([work.to_str().unwrap(), acme.to_str().unwrap()])
    );
    assert_eq!(document["next_action"], "af provider detect");

    let doctor = machine.af(&["doctor", "--json"]);
    let document: Value = serde_json::from_str(out(&doctor).trim()).unwrap();
    let providers = document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "providers")
        .unwrap();
    assert!(
        providers["detail"]
            .as_str()
            .unwrap()
            .ends_with(&format!("; found but not registered: {shown}")),
        "{providers}"
    );

    let repo = machine.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        ["init", "-q", "-b", "main"].as_slice(),
        [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first",
        ]
        .as_slice(),
    ] {
        let status = Command::new("git")
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    // The providers block is text only: the onboarding document keeps its shape.
    let onboard = machine
        .command(&["onboard", "--gate", "check=true"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(code(&onboard), 0, "{}", err(&onboard));
    let lines = out(&onboard);
    assert!(
        lines.contains(&format!("found but not registered: {shown} (not probed)")),
        "{lines}"
    );
}

/// A Claude CLI whose context is `$CLAUDE_CONFIG_DIR`, or `~/.claude` without the variable, as
/// the official one's is: logged in where that directory holds `logged-in`.
const DEFAULT_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' '2.1.299 (Claude Code)'
  exit 0
fi
if [ "$1" = auth ] && [ "$2" = status ] && [ "$3" = --json ]; then
  if [ -f "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/logged-in" ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'
  exit 1
fi
exit 64
"#;

/// GA final fix item 1: the default Claude context `af provider status` accepts — `~/.claude`
/// with no marker inside and a regular `~/.claude.json` beside it — is a candidate detect
/// probes and `--apply` registers, and status agrees about the registered Provider.
#[test]
fn detect_recognizes_the_default_claude_context_that_status_accepts() {
    let machine = Machine::new();
    machine.install("claude", DEFAULT_CLAUDE);
    // `logged-in` is the fake's login, not a marker: `~/.claude` holds none.
    let default = machine.dir(".claude", &["logged-in"]);

    // Without `~/.claude.json` it is an empty directory, left alone by both.
    let document = detected(&machine.af(&["provider", "detect", "--json"]));
    let row = candidate(&document, &default);
    assert_eq!(row["auth"], "not_probed", "{row}");
    assert_eq!(row["markers"], serde_json::json!([]), "{row}");

    std::fs::write(machine.home.join(".claude.json"), "{}").unwrap();
    let status = machine.af(&["provider", "status", "--json"]);
    let status: Value = serde_json::from_str(out(&status).trim()).unwrap();
    assert_eq!(status["providers"][0]["id"], "claude-ambient", "{status}");
    assert_eq!(status["providers"][0]["auth"], "authenticated", "{status}");

    let scan = machine.af(&["provider", "detect", "--json"]);
    assert_eq!(code(&scan), 0, "{}", err(&scan));
    let document = detected(&scan);
    let row = candidate(&document, &default);
    assert_eq!(row["source"], "default", "{row}");
    assert_eq!(
        row["markers"],
        serde_json::json!(["~/.claude.json"]),
        "{row}"
    );
    assert_eq!(row["auth"], "authenticated", "{row}");
    assert_eq!(row["suggested"]["action"], "register", "{row}");
    assert_eq!(row["suggested"]["id"], "claude-main", "{row}");
    let table = out(&machine.af(&["provider", "detect"]));
    assert!(
        table.contains("af provider add claude-main --kind claude --auth-dir ~/.claude"),
        "{table}"
    );
    assert!(machine.registered().is_empty());

    let apply = machine.af(&["provider", "detect", "--apply", "--json"]);
    assert_eq!(code(&apply), 0, "{}", err(&apply));
    let document = detected(&apply);
    let row = candidate(&document, &default);
    assert_eq!(row["applied"]["result"], "registered", "{row}");
    assert_eq!(machine.registered(), ["claude-main"]);

    // The registered Provider is the same context: status probes it and finds the login.
    let status = machine.af(&["provider", "status", "--json"]);
    assert_eq!(code(&status), 0, "{}", err(&status));
    let status: Value = serde_json::from_str(out(&status).trim()).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("provider-status-v3.json"),
        &status,
    );
    let main = status["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "claude-main")
        .unwrap_or_else(|| panic!("{status}"));
    assert_eq!(main["auth"], "authenticated", "{status}");
}

/// GA final fix item 6: with one kind's CLI found and the other's missing, `install` names the
/// missing one and there is no rerun; with no CLI at all the rerun keeps the scan's `--dir`.
#[test]
fn detect_names_the_missing_kind_and_the_rerun_as_typed_fields() {
    let machine = Machine::new();
    machine.install("claude", CLAUDE);
    machine.dir(".config/claude-work", &[".claude.json", "logged-in"]);
    let output = machine.af(&["provider", "detect", "--json"]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    let document = detected(&output);
    assert_eq!(
        document["install"],
        serde_json::json!([{
            "kind": "codex",
            "command": "npm install -g @openai/codex",
            "docs": "https://developers.openai.com/codex/cli",
        }]),
        "{document}"
    );
    assert_eq!(document["next_action"], Value::Null, "{document}");
    // The notes keep saying it to a person.
    assert!(
        document["notes"].as_array().unwrap().iter().any(|note| note
            .as_str()
            .unwrap()
            .contains("npm install -g @openai/codex")),
        "{document}"
    );

    let empty = Machine::new();
    let team = empty.root.join("secure team");
    let output = empty.af(&[
        "provider",
        "detect",
        "--kind",
        "claude",
        "--dir",
        team.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code(&output), 0, "{}", err(&output));
    let document = detected(&output);
    assert_eq!(document["install"][0]["kind"], "claude", "{document}");
    assert_eq!(
        document["install"].as_array().unwrap().len(),
        1,
        "{document}"
    );
    assert_eq!(
        document["next_action"],
        format!(
            "af provider detect --kind claude --dir '{}'",
            team.display()
        ),
        "{document}"
    );
}
