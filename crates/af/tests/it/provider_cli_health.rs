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

use crate::provider_cli::with_help;

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
        machine.install("codex", &with_help("codex", WORKING_CODEX), 0o755);
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
        &crate::schemas::validator("provider-status-v2.json"),
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

    let program = machine.install("codex", &with_help("codex", WORKING_CODEX), 0o644);
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
        &with_help(
            "codex",
            "#!/bin/sh\nif [ \"$1\" = login ]; then printf '%s\\n' 'Not logged in' >&2; fi\nexit 1\n",
        ),
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

/// Issue #259: Claude Code 2.1.101, authenticated, whose `--help` declares neither `--safe-mode`
/// nor `--restricted`. Status reports it `incompatible` under exit 4 — with its version, path,
/// both missing flags and the upgrade — and never as authenticated or as a login problem.
#[test]
fn status_reports_an_authenticated_claude_without_its_isolation_flags_as_incompatible() {
    use crate::provider_cli::{CLAUDE_HELP, with_page};
    const AUTHENTICATED_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' "$VERSION_FIXTURE"
  exit 0
fi
if [ "$1" = auth ] && [ "$2" = status ]; then
  printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"person@example.test","subscriptionType":"max"}'
  exit 0
fi
exit 64
"#;
    let old: String = CLAUDE_HELP
        .lines()
        .filter(|line| !line.contains("--safe-mode") && !line.contains("--restricted"))
        .map(|line| format!("{line}\n"))
        .collect();
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("claude-auth");
    std::fs::create_dir(&auth).unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let program = bin.join("claude");
    let status = |page: &str, version: &str| {
        let script = with_page(
            "claude",
            &AUTHENTICATED_CLAUDE.replace("$VERSION_FIXTURE", version),
            page,
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let run = |args: &[&str]| {
            crate::common::af()
                .args(args)
                .current_dir(root.path())
                .env("HOME", root.path())
                .env("XDG_CONFIG_HOME", root.path().join("config"))
                .env("CLAUDE_CONFIG_DIR", &auth)
                .env("PATH", &bin)
                .env("AF_SELF_OFFLINE", "1")
                .output()
                .unwrap()
        };
        (
            run(&["provider", "status", "--json"]),
            run(&["provider", "status"]),
        )
    };

    let (json, human) = status(&old, "2.1.101 (Claude Code)");
    assert_eq!(code(&json), 4, "{}", err(&json));
    let reported = document(&json);
    assert_eq!(reported["exit_code"], 4);
    let provider = only_provider(&reported);
    assert_eq!(provider["auth"], "incompatible");
    assert_eq!(provider["usability"], "unusable");
    // The versioned document carries the closed state only, never the CLI's words.
    assert!(!out(&json).contains("2.1.101"), "{}", out(&json));
    let program = std::fs::canonicalize(&program).unwrap();
    let expected = format!(
        "af provider status: provider claude-ambient: incompatible Provider CLI: the claude CLI `{}` (version 2.1.101 (Claude Code)) does not accept af's invocation; missing: --safe-mode, --restricted; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`",
        program.display()
    );
    let stderr = err(&json);
    assert!(stderr.contains(&expected), "{stderr}");
    assert!(!stderr.contains("login"), "{stderr}");
    assert_eq!(code(&human), 4, "{}", err(&human));
    let stdout = out(&human);
    assert!(stdout.contains("incompatible"), "{stdout}");
    assert!(
        stdout.contains("missing: --safe-mode, --restricted"),
        "{stdout}"
    );
    assert!(!stdout.contains("authenticated"), "{stdout}");

    // Claude Code 2.1.299 declares the whole invocation: the same login is authenticated.
    let (json, _) = status(CLAUDE_HELP, "2.1.299 (Claude Code)");
    assert_eq!(code(&json), 0, "{}", err(&json));
    let reported = document(&json);
    assert_eq!(only_provider(&reported)["auth"], "authenticated");
    assert!(err(&json).is_empty(), "{}", err(&json));
}

/// Issue #259: codex-cli 0.121.0 has no `--stdio`; its `app-server` serves stdio by default.
/// The usage probe starts the bare app-server and reads the plan. An app-server that exits before
/// answering `initialize` lacks the protocol capability: the compatibility check reports it
/// `incompatible` under exit 4 with its exit status and its own first error line, before any
/// login or usage question.
#[test]
fn the_codex_usage_probe_starts_the_bare_app_server_and_reports_an_early_exit() {
    const CODEX: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.121.0'
  exit 0
fi
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
if [ "$1" = app-server ] && { [ -n "$2" ] || [ -f "$CODEX_HOME/app-server-broken" ]; }; then
  printf '%s\n' "error: unexpected argument '${2:---stdio}' found" '' 'Usage: codex app-server [OPTIONS] [COMMAND]' '' "For more information, try '--help'." >&2
  exit 2
fi
if [ "$1" = app-server ]; then
  while IFS= read -r request; do
    case "$request" in
      *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
      *'"id":2'*) printf '%s\n' '{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":37,"windowDurationMins":10080,"resetsAt":1787831613},"secondary":null,"planType":"pro"}}}' ;;
    esac
  done
  exit 0
fi
exit 64
"#;
    let machine = Machine::registered();
    machine.install("codex", &with_help("codex", CODEX), 0o755);
    let probed = machine.af(&["provider", "status", "--json", "--usage"]);
    assert_eq!(code(&probed), 0, "{}", err(&probed));
    let reported = document(&probed);
    let provider = only_provider(&reported);
    assert_eq!(provider["auth"], "authenticated");
    assert_eq!(provider["usage"]["state"], "available");
    assert_eq!(provider["usage"]["windows"][0]["used_percent"], 37);

    std::fs::write(machine.auth.join("app-server-broken"), b"").unwrap();
    let human = machine.af(&["provider", "status", "--usage"]);
    assert_eq!(code(&human), 4, "{}", err(&human));
    let stdout = out(&human);
    assert!(stdout.contains("incompatible"), "{stdout}");
    assert!(
        err(&human).contains(&format!("missing: {APP_SERVER_PROTOCOL}; codex app-server exited 2 before answering initialize: error: unexpected argument '--stdio' found")),
        "{}",
        err(&human)
    );
    assert!(!stdout.contains("rate-limit response"), "{stdout}");
    assert!(
        !stdout.contains("subscription status unavailable"),
        "{stdout}"
    );
}

/// Issue #259, round 5: a Codex rate-limit snapshot whose `limitName` or `planType` looks like
/// credential material, with valid windows, reaches neither the status table, stderr nor the
/// JSON document: the label is the fixed withheld marker, the plan is unavailable, and the window
/// figures stay.
#[test]
fn credential_looking_codex_rate_limit_labels_are_withheld_on_status_usage() {
    const WITHHELD: &str = "[line withheld: looks like credential material]";
    for (label, secret) in [
        ("OPENAI_API_KEY=sk-proj-FIXTURE", "sk-proj-FIXTURE"),
        ("API_KEY=abcd", "abcd"),
    ] {
        let codex = format!(
            r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.149.0'
  exit 0
fi
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
if [ "$1" = app-server ]; then
  while IFS= read -r request; do
    case "$request" in
      *'"id":1'*) printf '%s\n' '{{"id":1,"result":{{}}}}' ;;
      *'"id":2'*) printf '%s\n' '{{"id":2,"result":{{"rateLimits":{{"limitId":"codex","limitName":"{label}","primary":{{"usedPercent":37,"windowDurationMins":10080,"resetsAt":1787831613}},"secondary":null,"planType":"{label}"}}}}}}' ;;
    esac
  done
  exit 0
fi
exit 64
"#
        );
        let machine = Machine::registered();
        machine.install("codex", &with_help("codex", &codex), 0o755);

        let json = machine.af(&["provider", "status", "--json", "--usage"]);
        assert_eq!(code(&json), 0, "{}", err(&json));
        let reported = document(&json);
        let provider = only_provider(&reported);
        assert_eq!(provider["usage"]["state"], "available", "{provider}");
        let window = &provider["usage"]["windows"][0];
        assert_eq!(window["used_percent"], 37, "{provider}");
        assert_eq!(window["window_minutes"], 10080, "{provider}");
        // The document carries no Provider-authored label at all.
        assert!(window.get("name").is_none(), "{provider}");

        let human = machine.af(&["provider", "status", "--usage"]);
        assert_eq!(code(&human), 0, "{}", err(&human));
        assert!(out(&human).contains(WITHHELD), "{}", out(&human));
        assert!(out(&human).contains("37%"), "{}", out(&human));
        for output in [&json, &human] {
            for said in [out(output), err(output)] {
                assert!(!said.contains(secret), "{label}: {said}");
                assert!(!said.contains("API_KEY"), "{label}: {said}");
            }
        }
    }
}

/// The protocol capability a Codex CLI lacks when its bare app-server cannot answer `initialize`.
const APP_SERVER_PROTOCOL: &str =
    "`codex app-server` answering JSON-RPC `initialize` over its default stdio transport";

/// A codex whose `exec --help` declares every option and whose login is ChatGPT, but whose bare
/// `app-server` exits 2 before answering `initialize`, with its own error line.
pub(crate) const CODEX_WITHOUT_APP_SERVER: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.149.0'
  exit 0
fi
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
if [ "$1" = app-server ]; then
  printf '%s\n' "error: unrecognized subcommand 'app-server'" '' 'Usage: codex [OPTIONS] [PROMPT]' >&2
  exit 2
fi
exit 64
"#;

/// Issue #259, round 2: authentication never implies compatibility. A codex whose help page is
/// complete and whose `login status` reports ChatGPT, but whose app-server exits before
/// `initialize`, is `incompatible` under exit 4 on default status and on `--usage` alike, naming
/// the protocol capability, the exit status and the CLI's own line, and setup refuses it with the
/// same report before registering it.
#[test]
fn a_codex_whose_app_server_exits_before_initialize_is_incompatible_on_status_and_setup() {
    let machine = Machine::registered();
    let program = machine.install(
        "codex",
        &with_help("codex", CODEX_WITHOUT_APP_SERVER),
        0o755,
    );
    let report = |provider: &str| {
        format!(
            "provider {provider}: incompatible Provider CLI: the codex CLI `{}` (version codex-cli 0.149.0) does not accept af's invocation; missing: {APP_SERVER_PROTOCOL}; codex app-server exited 2 before answering initialize: error: unrecognized subcommand 'app-server'; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`",
            program.display()
        )
    };
    for usage in [false, true] {
        let mut args = vec!["provider", "status", "--json"];
        if usage {
            args.push("--usage");
        }
        let json = machine.af(&args);
        assert_eq!(code(&json), 4, "{usage}: {}", err(&json));
        let reported = document(&json);
        assert_eq!(reported["exit_code"], 4);
        let provider = only_provider(&reported);
        assert_eq!(provider["auth"], "incompatible", "{usage}");
        assert_eq!(provider["usability"], "unusable", "{usage}");
        // The versioned document carries the closed state only, never the CLI's words.
        assert!(
            !out(&json).contains("unrecognized subcommand"),
            "{}",
            out(&json)
        );
        let stderr = err(&json);
        assert!(
            stderr.contains(&format!("af provider status: {}", report("codex-main"))),
            "{usage}: {stderr}"
        );
        assert!(!stderr.contains("login"), "{stderr}");

        args.retain(|arg| *arg != "--json");
        let human = machine.af(&args);
        assert_eq!(code(&human), 4, "{usage}: {}", err(&human));
        let stdout = out(&human);
        assert!(stdout.contains("incompatible"), "{stdout}");
        assert!(!stdout.contains("authenticated"), "{stdout}");
        assert!(!stdout.contains("usage unavailable"), "{stdout}");
        assert!(
            err(&human).contains(&report("codex-main")),
            "{}",
            err(&human)
        );
    }

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
    assert_eq!(
        reported["diagnostic"],
        format!(
            "provider codex-other: incompatible Provider CLI: the codex CLI `{}` does not accept af's invocation; missing: {APP_SERVER_PROTOCOL}",
            program.display()
        )
    );
    assert!(!stdout.contains("unrecognized subcommand"), "{stdout}");
    assert!(
        err(&setup).contains(&report("codex-other")),
        "{}",
        err(&setup)
    );
    let registry =
        std::fs::read_to_string(machine.root.path().join("config/af/providers.toml")).unwrap();
    assert!(!registry.contains("codex-other"), "{registry}");
}

/// A codex whose `exec --help` declares every option and whose login is ChatGPT, but whose bare
/// `app-server` answers `initialize` with `answer` and exits 0; with
/// `$CODEX_HOME/initialize-result` it first writes a notification and then answers
/// `{"id":1,"result":{}}` instead.
fn codex_answering_initialize(answer: &str) -> String {
    codex_answering_initialize_then(answer, "exit 0")
}

/// [`codex_answering_initialize`], whose app-server runs `then` after writing `answer`. Its
/// proper answer is preceded by a notification and a response to another id, both skipped.
fn codex_answering_initialize_then(answer: &str, then: &str) -> String {
    format!(
        r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.149.0'
  exit 0
fi
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
if [ "$1" = app-server ]; then
  IFS= read -r _
  if [ -f "$CODEX_HOME/initialize-result" ]; then
    printf '%s\n' '{{"method":"x/changed","params":{{}}}}' '{{"id":7,"result":{{}}}}' '{{"id":1,"result":{{}}}}'
    exit 0
  fi
  printf '%s\n' '{answer}'
  {then}
fi
exit 64
"#
    )
}

/// Issue #259, round 3: a reply with id 1 but no `result` object never proves the app-server
/// protocol. Such a codex is `incompatible` under exit 4 on status, with and without `--usage`,
/// and setup refuses it before registering it; the same codex answering a result object is
/// authenticated.
#[test]
fn a_codex_answering_initialize_without_a_result_is_incompatible_on_status_and_setup() {
    // The exit is reported once the app-server has ended, which it does right after answering.
    a_refused_initialize_is_incompatible_on_status_and_setup(
        r#"{"id":1}"#,
        "exited 0 after answering initialize without a result object",
    );
}

/// Issue #259, round 4: an app-server answering `initialize` with an error object and exiting 0
/// is `incompatible` under exit 4 on status, with and without `--usage`, and on setup, reported
/// with its exit status and the error line it answered with.
#[test]
fn a_codex_rejecting_initialize_is_incompatible_with_its_exit_and_line() {
    a_refused_initialize_is_incompatible_on_status_and_setup(
        r#"{"id":1,"error":{"code":-32600,"message":"bad init"}}"#,
        "exited 0 after answering initialize with an error",
    );
}

/// Issue #259, round 5: an app-server that writes `{"foo":"bar"}`, neither a response with an
/// `id` nor a notification with a `method`, before any `initialize` answer and stays running is
/// refused at once, never waited on as silence: `incompatible` under exit 4 on status, with and
/// without `--usage`, and on setup, within the probe timeout, reporting that af stopped it and the
/// line it wrote. The same app-server writing a notification before its result is accepted.
///
/// Round 6: every line before the answer passes the one JSON-RPC line grammar, so an object with
/// an `id` but neither `result` nor `error` (`{"id":2,"foo":"bar"}`), an array and prose are
/// refused the same way, while a notification and a response to another id before the result are
/// accepted (the accepted half of each case).
#[test]
fn a_codex_writing_a_malformed_object_before_initialize_is_incompatible_at_once() {
    for line in [
        r#"{"foo":"bar"}"#,
        r#"{"id":2,"foo":"bar"}"#,
        "[1,2]",
        "not json",
    ] {
        a_refused_initialize_is_incompatible_on_status_and_setup_with(
            &codex_answering_initialize_then(line, "while IFS= read -r _; do :; done\n  exit 0"),
            line,
            "was stopped by af after answering initialize with a line that is not a JSON-RPC message",
        );
    }
}

/// Issue #259, round 6: the lines after the `initialize` answer pass the same grammar. An
/// app-server that answers `initialize` but, still running, writes `{"id":2,"foo":"bar"}` in place
/// of its rate-limit response is `incompatible` under exit 4 on `status --usage` at once, with
/// the line and the report that af stopped it, never a usage timeout; plain status, which asks
/// only for `initialize`, stays authenticated.
#[test]
fn a_codex_writing_a_malformed_line_before_its_rate_limit_answer_is_incompatible_at_once() {
    const CODEX: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.149.0'
  exit 0
fi
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
if [ "$1" = app-server ]; then
  while IFS= read -r request; do
    case "$request" in
      *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
      *'"id":2'*) printf '%s\n' '{"method":"x/changed","params":{}}' '{"id":7,"result":{}}' '{"id":2,"foo":"bar"}' ;;
    esac
  done
  exit 0
fi
exit 64
"#;
    let machine = Machine::registered();
    let program = machine.install("codex", &with_help("codex", CODEX), 0o755);
    let plain = machine.af(&["provider", "status", "--json"]);
    assert_eq!(code(&plain), 0, "{}", err(&plain));
    assert_eq!(only_provider(&document(&plain))["auth"], "authenticated");
    let report = format!(
        "provider codex-main: incompatible Provider CLI: the codex CLI `{}` (version codex-cli 0.149.0) does not accept af's invocation; missing: {APP_SERVER_PROTOCOL}; codex app-server was stopped by af after answering initialize, before answering `account/rateLimits/read`, with a line that is not a JSON-RPC message: {{\"id\":2,\"foo\":\"bar\"}}; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`",
        program.display()
    );
    for json in [true, false] {
        let mut args = vec!["provider", "status", "--usage"];
        if json {
            args.push("--json");
        }
        let started = std::time::Instant::now();
        let output = machine.af(&args);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "waited {:?}",
            started.elapsed()
        );
        assert_eq!(code(&output), 4, "{}", err(&output));
        assert!(err(&output).contains(&report), "{}", err(&output));
        if json {
            let provider = document(&output)["providers"][0].clone();
            assert_eq!(provider["auth"], "incompatible");
            assert_eq!(provider["usability"], "unusable");
        } else {
            assert!(out(&output).contains("incompatible"), "{}", out(&output));
            assert!(!out(&output).contains("authenticated"), "{}", out(&output));
        }
    }
}

/// Issue #259, round 7: a response answers a request only if af read it after writing the
/// request. An app-server that writes a well-formed rate-limit answer before its `initialize`
/// answer, then exits on af's `account/rateLimits/read` without answering it, leaves usage
/// unavailable on `status --usage`: the premature limits are never shown. The same app-server
/// answering after the request is shown.
#[test]
fn a_codex_rate_limit_answer_written_before_the_request_is_never_shown() {
    const CODEX: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.149.0'
  exit 0
fi
if [ "$1" = login ] && [ "$2" = status ]; then
  printf '%s\n' 'Logged in using ChatGPT' >&2
  exit 0
fi
if [ "$1" = app-server ]; then
  while IFS= read -r request; do
    case "$request" in
      *'"id":1'*) printf '%s\n' '{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":91,"windowDurationMins":10080,"resetsAt":1787831613},"secondary":null,"planType":"pro"}}}' '{"id":1,"result":{}}' ;;
      *'"id":2'*)
        if [ -f "$CODEX_HOME/answers-in-order" ]; then
          printf '%s\n' '{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":37,"windowDurationMins":10080,"resetsAt":1787831613},"secondary":null,"planType":"pro"}}}'
        else
          exit 0
        fi ;;
    esac
  done
  exit 0
fi
exit 64
"#;
    let machine = Machine::registered();
    machine.install("codex", &with_help("codex", CODEX), 0o755);
    let premature = machine.af(&["provider", "status", "--json", "--usage"]);
    assert_eq!(code(&premature), 7, "{}", err(&premature));
    let reported = document(&premature);
    let provider = only_provider(&reported);
    assert_eq!(provider["auth"], "authenticated", "{provider}");
    assert_eq!(provider["usage"]["state"], "unavailable", "{provider}");
    assert_eq!(
        provider["usage"]["windows"],
        serde_json::json!([]),
        "{provider}"
    );
    let human = machine.af(&["provider", "status", "--usage"]);
    assert_eq!(code(&human), 7, "{}", err(&human));
    assert!(
        out(&human).contains(
            "subscription status unavailable: codex app-server exited 0 after initialize without answering `account/rateLimits/read`"
        ),
        "{}",
        out(&human)
    );
    assert!(!out(&human).contains("91%"), "{}", out(&human));
    assert!(!out(&human).contains("ChatGPT Pro"), "{}", out(&human));

    std::fs::write(machine.auth.join("answers-in-order"), b"").unwrap();
    let in_order = machine.af(&["provider", "status", "--json", "--usage"]);
    assert_eq!(code(&in_order), 0, "{}", err(&in_order));
    let reported = document(&in_order);
    let provider = only_provider(&reported);
    assert_eq!(provider["usage"]["state"], "available", "{provider}");
    assert_eq!(provider["usage"]["windows"][0]["used_percent"], 37);
}

/// `answer` refuses the protocol capability as `refusal` says; the report carries it, then the
/// line itself.
fn a_refused_initialize_is_incompatible_on_status_and_setup(answer: &str, refusal: &str) {
    a_refused_initialize_is_incompatible_on_status_and_setup_with(
        &codex_answering_initialize(answer),
        answer,
        refusal,
    );
}

/// [`a_refused_initialize_is_incompatible_on_status_and_setup`] for the codex `script`, which
/// writes `answer` as the first line of its app-server. Each refusal arrives well within the
/// production probe timeout, though tests raise it: none is a timeout.
fn a_refused_initialize_is_incompatible_on_status_and_setup_with(
    script: &str,
    answer: &str,
    refusal: &str,
) {
    const PRODUCTION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    let machine = Machine::registered();
    machine.install("codex", &with_help("codex", script), 0o755);
    let af = |args: &[&str]| {
        let started = std::time::Instant::now();
        let output = machine.af(args);
        assert!(
            started.elapsed() < PRODUCTION_PROBE_TIMEOUT,
            "{args:?} waited {:?}: {}",
            started.elapsed(),
            err(&output)
        );
        output
    };
    let observed = format!(
        "missing: {APP_SERVER_PROTOCOL}; codex app-server {refusal}: {answer}; fix: upgrade it with `npm install -g @openai/codex@latest`"
    );
    for usage in [false, true] {
        let mut args = vec!["provider", "status", "--json"];
        if usage {
            args.push("--usage");
        }
        let json = af(&args);
        assert_eq!(code(&json), 4, "{usage}: {}", err(&json));
        let reported = document(&json);
        assert_eq!(reported["exit_code"], 4);
        let provider = only_provider(&reported);
        assert_eq!(provider["auth"], "incompatible", "{usage}");
        assert_eq!(provider["usability"], "unusable", "{usage}");
        assert!(err(&json).contains(&observed), "{usage}: {}", err(&json));
        assert!(!err(&json).contains("login"), "{}", err(&json));

        args.retain(|arg| *arg != "--json");
        let human = af(&args);
        assert_eq!(code(&human), 4, "{usage}: {}", err(&human));
        assert!(out(&human).contains("incompatible"), "{}", out(&human));
        assert!(!out(&human).contains("authenticated"), "{}", out(&human));
    }

    let other = machine.root.path().join("codex-other");
    let setup = af(&[
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
    let reported: Value = serde_json::from_str(out(&setup).trim()).unwrap();
    assert_eq!(reported["result"], "provider_cli_missing");
    assert!(err(&setup).contains(&observed), "{}", err(&setup));
    let registry =
        std::fs::read_to_string(machine.root.path().join("config/af/providers.toml")).unwrap();
    assert!(!registry.contains("codex-other"), "{registry}");

    // A result object, however empty, proves the protocol.
    std::fs::write(machine.auth.join("initialize-result"), b"").unwrap();
    let json = af(&["provider", "status", "--json"]);
    assert_eq!(code(&json), 0, "{}", err(&json));
    assert_eq!(
        only_provider(&document(&json))["auth"],
        "authenticated",
        "{}",
        err(&json)
    );
}

/// Setup refuses an incompatible CLI before asking it about a login, under the closed
/// `provider_cli_missing` result and exit 4: the CLI as installed cannot serve af (ADR-0146).
#[test]
fn setup_refuses_an_incompatible_cli_before_any_login() {
    use crate::provider_cli::{CODEX_EXEC_HELP, with_page};
    let machine = Machine::registered();
    let old: String = CODEX_EXEC_HELP
        .lines()
        .filter(|line| !line.contains("--ephemeral"))
        .map(|line| format!("{line}\n"))
        .collect();
    machine.install("codex", &with_page("codex", WORKING_CODEX, &old), 0o755);
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
    assert_eq!(reported["provider"]["auth"], "unknown");
    let diagnostic = reported["diagnostic"].as_str().unwrap();
    assert!(
        diagnostic.starts_with("provider codex-other: incompatible Provider CLI: the codex CLI")
            && diagnostic.ends_with("does not accept af's invocation; missing: --ephemeral"),
        "{diagnostic}"
    );
    // The CLI's own version is Provider-authored: stderr only.
    assert!(!stdout.contains("codex-cli 0.149.0"), "{stdout}");
    let stderr = err(&setup);
    assert!(stderr.contains("(version codex-cli 0.149.0)"), "{stderr}");
    assert!(
        stderr.contains("npm install -g @openai/codex@latest"),
        "{stderr}"
    );
    let registry =
        std::fs::read_to_string(machine.root.path().join("config/af/providers.toml")).unwrap();
    assert!(!registry.contains("codex-other"), "{registry}");
}
