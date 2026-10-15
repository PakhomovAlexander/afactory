//! `af provider models` against fake CLIs serving the two recorded catalogs (issue #258,
//! ADR-0148): the table, the `af/provider-models@1` document, every `--resolve` case, and exits
//! 4 and 7. No catalog read starts a turn.

use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;

use crate::fake_models;

struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    /// A machine with `claude-work` (Claude) and `codex-main` (Codex) registered, each served by
    /// its fake CLI.
    fn new() -> Self {
        let machine = Self {
            root: tempfile::tempdir().unwrap(),
        };
        fake_models::install("claude", &machine.bin(), &machine.auth("claude"));
        fake_models::install("codex", &machine.bin(), &machine.auth("codex"));
        let registry = serde_json::json!({"version": 1, "providers": [
            {"id": "claude-work", "kind": "claude", "auth_dir": machine.auth("claude")},
            {"id": "codex-main", "kind": "codex", "auth_dir": machine.auth("codex")},
        ]});
        std::fs::write(machine.registry(), toml::to_string(&registry).unwrap()).unwrap();
        machine
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn auth(&self, kind: &str) -> PathBuf {
        self.root.path().join(format!("{kind}-auth"))
    }

    fn registry(&self) -> PathBuf {
        self.root.path().join("providers.toml")
    }

    fn af(&self, args: &[&str]) -> Output {
        crate::common::af()
            .arg("provider")
            .arg("models")
            .args(args)
            .current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("USER", "fixture")
            .env("PATH", self.bin())
            .env("AF_PROVIDERS_FILE", self.registry())
            .env("AF_SELF_OFFLINE", "1")
            .output()
            .unwrap()
    }

    /// No catalog read sent the Claude session anything but control requests.
    fn assert_no_turn(&self) {
        assert_eq!(
            fake_models::turns(&self.auth("claude")),
            "",
            "a turn started"
        );
    }
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn document(output: &Output) -> Value {
    let stdout = out(output);
    let document: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout is not one document: {error}\n{stdout}"));
    crate::schemas::valid(
        &crate::schemas::validator("provider-models-v1.json"),
        &document,
    );
    document
}

#[test]
fn the_table_lists_both_providers_with_source_version_freshness_rows_and_proof() {
    let machine = Machine::new();
    let listed = machine.af(&[]);
    assert_eq!(listed.status.code(), Some(0), "{}", err(&listed));
    let text = out(&listed);
    for part in [
        "claude-work (claude)  source claude list_models  version 2.1.300  freshness live",
        "codex-main (codex)  source codex app-server model/list  version 0.162.1  freshness models_cache.json fetched_at 2026-10-15T09:58:12Z",
        "af recognises these spellings for this Provider",
        "whether this account can run one is proven only by the charged `af provider doctor`",
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    let row = |id: &str| {
        text.lines()
            .find(|line| line.trim_start().starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("no row for {id}:\n{text}"))
            .to_owned()
    };
    for (id, parts) in [
        (
            "claude-opus-5-5",
            &["Opus 5.5", "default, opus", "low medium high xhigh max"][..],
        ),
        ("claude-fable-5-1", &["Fable 5.1", "fable"][..]),
        (
            "gpt-6.1-sol",
            &["GPT-6.1-Sol", "low* medium high xhigh max ultra"][..],
        ),
        (
            "gpt-6-luna",
            &["GPT-6-Luna", "low medium* high xhigh max"][..],
        ),
    ] {
        let line = row(id);
        for part in parts {
            assert!(line.contains(part), "{part}: {line}");
        }
    }
    assert!(!row("gpt-6-luna").contains("ultra"));
    // Only the token-free catalog sessions ran: no turn, no `-p` call with a prompt.
    machine.assert_no_turn();
    let started = fake_models::started(&machine.auth("claude"));
    assert!(
        started.lines().all(|line| [
            "--help",
            "-p --input-format stream-json --output-format stream-json --verbose"
        ]
        .contains(&line)),
        "{started}"
    );
}

#[test]
fn the_document_validates_and_carries_every_row() {
    let machine = Machine::new();
    let output = machine.af(&["--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    let document = document(&output);
    assert_eq!(document["schema"], "af/provider-models@1");
    assert_eq!(document["exit_code"], 0);
    assert_eq!(document["token_free"], true);
    let providers = document["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 2);
    let claude = &providers[0];
    assert_eq!(claude["id"], "claude-work");
    assert_eq!(claude["state"], "listed");
    assert_eq!(claude["harness_version"], "2.1.300");
    assert_eq!(claude["freshness"]["source"], "live");
    let fable = claude["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "claude-fable-5-1")
        .unwrap();
    assert_eq!(fable["aliases"], serde_json::json!(["fable"]));
    assert_eq!(fable["display_name"], "Fable 5.1");
    let codex = &providers[1];
    assert_eq!(codex["freshness"]["fetched_at"], "2026-10-15T09:58:12Z");
    assert_eq!(codex["models"][0]["default_effort"], "low");
    // The account's email the fakes know is never in the document.
    assert!(!out(&output).contains("example.test"));
    machine.assert_no_turn();
}

#[test]
fn resolve_on_the_claude_provider() {
    let machine = Machine::new();
    let resolve = |spelling: &str| {
        let output = machine.af(&["claude-work", "--resolve", spelling]);
        assert_eq!(output.status.code(), Some(0), "{}", err(&output));
        out(&output)
    };
    let text = resolve("Fable 5.1/xhigh");
    for part in [
        "`Fable 5.1` is the display name of the exact ID claude-fable-5-1",
        "xhigh: confirmed, claude-fable-5-1 lists low, medium, high, xhigh, max",
        "reviewer.toml  [runner] program = \"claude\"  args = [{ value = \"--model\" }, { value = \"claude-fable-5-1\" }, { value = \"--effort\" }, { value = \"xhigh\" }]",
        "worker.toml    [runner]",
        "provider_kind = \"claude\"",
        "model = \"claude-fable-5-1\"",
        "effort = \"xhigh\"",
        "bindings       [providers] \"PACKAGE\" = \"claude-work\"",
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    assert!(
        !text.contains("codex-main"),
        "only the named Provider: {text}"
    );
    let text = resolve("fable");
    assert!(
        text.contains("`fable` is an alias of the exact ID claude-fable-5-1; af binds exact IDs"),
        "{text}"
    );
    let text = resolve("claude-fable-5.1");
    assert!(
        text.contains("`claude-fable-5.1` is not in this harness's catalog; nearest catalog IDs: claude-fable-5-1"),
        "{text}"
    );
    let text = resolve("claude-opus-4-8");
    assert!(
        text.contains("`claude-opus-4-8` is not in this harness's catalog; passes af's spelling rule; usability only by af provider doctor"),
        "{text}"
    );
    let output = machine.af(&["claude-work", "--json", "--resolve", "Fable 5.1/xhigh"]);
    let resolution = &document(&output)["providers"][0]["resolution"];
    assert_eq!(resolution["outcome"], "display_name");
    assert_eq!(resolution["id"], "claude-fable-5-1");
    assert_eq!(resolution["effort_check"], "listed");
    assert!(
        resolution["configuration"]["worker_runner"]
            .as_str()
            .unwrap()
            .contains("model = \"claude-fable-5-1\"")
    );
    machine.assert_no_turn();
}

#[test]
fn resolve_on_the_codex_provider() {
    let machine = Machine::new();
    let output = machine.af(&["codex-main", "--resolve", "GPT-6 Sol/ultra"]);
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));
    let text = out(&output);
    for part in [
        "`GPT-6 Sol` names the exact ID gpt-6-sol",
        "ultra: confirmed, gpt-6-sol lists low, medium, high, xhigh, max, ultra",
        "{ value = \"model_reasoning_effort=\\\"ultra\\\"\" }",
        "provider_kind = \"codex\"",
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    let output = machine.af(&["codex-main", "--json", "--resolve", "gpt-6-luna/ultra"]);
    let resolution = &document(&output)["providers"][0]["resolution"];
    assert_eq!(resolution["outcome"], "exact");
    assert_eq!(resolution["effort_check"], "not_listed");
    assert_eq!(
        resolution["configuration"],
        Value::Null,
        "af never substitutes"
    );
    let text = out(&machine.af(&["codex-main", "--resolve", "gpt-6-luna/ultra"]));
    assert!(
        text.contains(
            "ultra: effort not listed for gpt-6-luna, which lists low, medium, high, xhigh, max"
        ),
        "{text}"
    );
}

#[test]
fn an_unavailable_catalog_exits_7_and_prints_the_static_rules() {
    let machine = Machine::new();
    std::fs::write(machine.auth("claude").join("catalog-broken"), "").unwrap();
    let output = machine.af(&[]);
    assert_eq!(output.status.code(), Some(7), "{}", err(&output));
    let text = out(&output);
    assert!(
        text.contains("claude-work (claude)  catalog unavailable"),
        "{text}"
    );
    assert!(
        text.contains("claude effort levels: low, medium, high, xhigh, max"),
        "{text}"
    );
    assert!(
        !text.contains("claude-fable-5-1   "),
        "nothing invented: {text}"
    );
    // The other Provider is still listed.
    assert!(text.contains("gpt-6-sol"), "{text}");
    assert!(
        err(&output).contains(
            "af provider models: claude-work: claude exited 1 before answering `list_models`"
        ),
        "{}",
        err(&output)
    );
    let json = document(&machine.af(&["--json"]));
    assert_eq!(json["exit_code"], 7);
    assert_eq!(json["providers"][0]["state"], "unavailable");
}

#[test]
fn a_cli_that_cannot_start_exits_4() {
    let machine = Machine::new();
    std::fs::remove_file(machine.bin().join("codex")).unwrap();
    let output = machine.af(&["--json"]);
    assert_eq!(output.status.code(), Some(4), "{}", err(&output));
    let json = document(&output);
    assert_eq!(json["exit_code"], 4);
    assert_eq!(json["providers"][1]["state"], "cli_refused");
    assert_eq!(json["providers"][0]["state"], "listed");
    let unknown = machine.af(&["nobody"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(err(&unknown).contains("provider `nobody` is not listed"));
}
