//! A Provider whose official CLI cannot start (issue #136, ADR-0130).
//!
//! An auto-update once left `codex` without its platform package: every invocation failed at
//! once with the CLI's own installation error, and af reported nothing that pointed at the CLI.
//! These fixture programs reproduce the three shapes — a CLI that exits 1 printing an
//! installation error, a missing program, and a working CLI — and pin what `af provider status`
//! and `af provider setup` say about each, on which stream, under which exit code.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;

const NPM_ERROR: &str = "Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall Codex: npm install -g @openai/codex@latest";
const NPM_FIX: &str = "fix: Reinstall Codex: npm install -g @openai/codex@latest";

/// What Node prints when the npm package lost its platform binary: a stack frame first, then the
/// error, for every argument the CLI is given.
fn broken_codex() -> String {
    format!(
        "#!/bin/sh\nprintf '%s\\n' 'file:///usr/lib/node_modules/@openai/codex/bin/codex.js:100' '    throw new Error(' '' '{NPM_ERROR}' >&2\nexit 1\n"
    )
}

/// A working Codex CLI: it answers its version and its login status.
const WORKING_CODEX: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.149.0'
  exit 0
fi
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
exit 64
"#;

struct Machine {
    root: tempfile::TempDir,
    auth: PathBuf,
}

impl Machine {
    /// A home whose registry holds `codex-main`, registered while a working CLI was installed.
    fn registered() -> Self {
        let root = tempfile::tempdir().unwrap();
        let auth = root.path().join("codex-auth");
        std::fs::create_dir(&auth).unwrap();
        let machine = Self { root, auth };
        machine.install("codex", WORKING_CODEX, 0o755);
        let added = machine.af(&[
            "provider",
            "add",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            machine.auth.to_str().unwrap(),
        ]);
        assert_eq!(code(&added), 0, "{}", err(&added));
        machine
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn install(&self, name: &str, script: &str, mode: u32) -> PathBuf {
        std::fs::create_dir_all(self.bin()).unwrap();
        let path = self.bin().join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        // The resolved program is reported by its canonical path.
        std::fs::canonicalize(&path).unwrap()
    }

    /// `af` on a `PATH` holding only the fixture programs, with the registered context as the
    /// ambient one too, so every document has exactly one Provider.
    fn af(&self, args: &[&str]) -> Output {
        crate::common::af()
            .args(args)
            // Outside any checkout, so no repository's af pin is consulted.
            .current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("CODEX_HOME", &self.auth)
            .env("PATH", self.bin())
            .env("AF_SELF_OFFLINE", "1")
            .output()
            .unwrap()
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

fn document(output: &Output) -> Value {
    let stdout = out(output);
    let document: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout is not one document: {error}\n{stdout}"));
    crate::schemas::valid(
        &crate::schemas::validator("provider-status-v1.json"),
        &document,
    );
    document
}

fn only_provider(document: &Value) -> &Value {
    let providers = document["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "{document}");
    &providers[0]
}

#[test]
fn status_reports_a_cli_that_exits_on_its_version_check_as_an_installation_failure() {
    let machine = Machine::registered();
    let program = machine.install("codex", &broken_codex(), 0o755);

    let json = machine.af(&["provider", "status", "--json"]);
    assert_eq!(code(&json), 4, "{}", err(&json));
    let reported = document(&json);
    assert_eq!(reported["exit_code"], 4);
    let provider = only_provider(&reported);
    assert_eq!(provider["id"], "codex-main");
    assert_eq!(provider["auth"], "installation_failed");
    assert_eq!(provider["usability"], "unusable");
    // The CLI's own words are Provider-authored: stderr carries them, the document never does.
    assert!(
        !out(&json).contains("Missing optional dependency"),
        "{}",
        out(&json)
    );
    let stderr = err(&json);
    let expected = format!(
        "af provider status: provider codex-main: Provider installation failure: the codex CLI `{}` exited 1 on its own `--version` check before doing any provider work: {NPM_ERROR}; {NPM_FIX}",
        program.display()
    );
    assert!(stderr.contains(&expected), "{stderr}");

    let human = machine.af(&["provider", "status"]);
    assert_eq!(code(&human), 4, "{}", err(&human));
    let stdout = out(&human);
    assert!(stdout.contains("installation failed"), "{stdout}");
    assert!(stdout.contains(NPM_ERROR), "{stdout}");
    assert!(stdout.contains(NPM_FIX), "{stdout}");
    assert!(!stdout.contains("not authenticated"), "{stdout}");
}

#[test]
fn status_reports_a_missing_or_non_executable_cli_as_an_installation_failure() {
    let machine = Machine::registered();
    std::fs::remove_file(machine.bin().join("codex")).unwrap();
    let missing = machine.af(&["provider", "status", "--json"]);
    assert_eq!(code(&missing), 4, "{}", err(&missing));
    let reported = document(&missing);
    assert_eq!(only_provider(&reported)["auth"], "installation_failed");
    let stderr = err(&missing);
    assert!(
        stderr.contains(
            "provider codex-main: Provider installation failure: the codex CLI `codex` is not on PATH; fix: install the official codex CLI"
        ),
        "{stderr}"
    );

    let program = machine.install("codex", WORKING_CODEX, 0o644);
    let unexecutable = machine.af(&["provider", "status"]);
    assert_eq!(code(&unexecutable), 4, "{}", err(&unexecutable));
    let stderr = err(&unexecutable);
    assert!(
        stderr.contains(&format!(
            "provider codex-main: Provider installation failure: the codex CLI `{}` is not executable",
            program.display()
        )),
        "{stderr}"
    );
}

#[test]
fn status_passes_a_working_cli_and_keeps_a_logged_out_one_an_authentication_answer() {
    let machine = Machine::registered();
    let working = machine.af(&["provider", "status", "--json"]);
    assert_eq!(code(&working), 0, "{}", err(&working));
    let reported = document(&working);
    assert_eq!(only_provider(&reported)["auth"], "authenticated");
    assert!(err(&working).is_empty(), "{}", err(&working));

    // A CLI that answered its probe has started, even if it rejects `--version`: never diagnosed.
    machine.install(
        "codex",
        "#!/bin/sh\nif [ \"$1\" = login ]; then printf '%s\\n' 'Not logged in' >&2; fi\nexit 1\n",
        0o755,
    );
    let logged_out = machine.af(&["provider", "status", "--json"]);
    assert_eq!(code(&logged_out), 0, "{}", err(&logged_out));
    let reported = document(&logged_out);
    assert_eq!(only_provider(&reported)["auth"], "not_authenticated");
}

#[test]
fn a_claude_cli_that_cannot_start_is_reported_the_same_way() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("claude-auth");
    std::fs::create_dir(&auth).unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let program = bin.join("claude");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' 'Error: Cannot find module ../cli.js' 'Reinstall Claude Code: npm install -g @anthropic-ai/claude-code' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = crate::common::af()
        .args(["provider", "status", "--json"])
        .current_dir(root.path())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("CLAUDE_CONFIG_DIR", &auth)
        .env("PATH", &bin)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert_eq!(code(&status), 4, "{}", err(&status));
    let reported = document(&status);
    assert_eq!(only_provider(&reported)["auth"], "installation_failed");
    let stderr = err(&status);
    assert!(
        stderr.contains("the claude CLI")
            && stderr.contains(": Error: Cannot find module ../cli.js; fix: Reinstall Claude Code: npm install -g @anthropic-ai/claude-code"),
        "{stderr}"
    );
}

/// Setup used to call this a failed login (exit 6); the login was never the problem.
#[test]
fn setup_refuses_a_cli_that_cannot_start_as_a_missing_provider_cli() {
    let machine = Machine::registered();
    machine.install("codex", &broken_codex(), 0o755);
    let other = machine.root.path().join("codex-other");
    let setup = machine.af(&[
        "provider",
        "setup",
        "codex-other",
        "--kind",
        "codex",
        "--auth-dir",
        other.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code(&setup), 4, "{}", err(&setup));
    let stdout = out(&setup);
    let reported: Value = serde_json::from_str(stdout.trim()).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("provider-setup-v1.json"),
        &reported,
    );
    assert_eq!(reported["result"], "provider_cli_missing");
    let diagnostic = reported["diagnostic"].as_str().unwrap();
    assert!(
        diagnostic.starts_with("provider codex-other: Provider installation failure"),
        "{diagnostic}"
    );
    assert!(!stdout.contains("Missing optional dependency"), "{stdout}");
    let stderr = err(&setup);
    assert!(stderr.contains(NPM_ERROR), "{stderr}");
    assert!(stderr.contains("(provider_cli_missing)"), "{stderr}");
    let registry =
        std::fs::read_to_string(machine.root.path().join("config/af/providers.toml")).unwrap();
    assert!(!registry.contains("codex-other"), "{registry}");
}
