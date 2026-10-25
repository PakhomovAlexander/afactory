//! `af provider models` against fake CLIs serving the two recorded catalogs (issue #258,
//! ADR-0148): the table, the `af/provider-models@1` document, every `--resolve` case, and exits
//! 4 and 7. No catalog read starts a turn.

use std::collections::BTreeMap;
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

/// The width of the label column `--resolve` prints each configuration block under.
const LABEL_COLUMN: usize = 19;

/// The configuration blocks `--resolve` printed, by label (`reviewer.toml`, `worker.toml`,
/// `bindings`), each with the label column removed: exactly the lines a person pastes.
fn printed_blocks(text: &str) -> BTreeMap<String, String> {
    let mut blocks = BTreeMap::<String, Vec<String>>::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let label = line
            .get(..LABEL_COLUMN)
            .filter(|column| column.starts_with("    "))
            .map(str::trim);
        match label {
            Some(label @ ("reviewer.toml" | "worker.toml" | "bindings")) => {
                assert!(
                    blocks
                        .insert(label.to_owned(), vec![line[LABEL_COLUMN..].to_owned()])
                        .is_none(),
                    "{label} printed twice:\n{text}"
                );
                current = Some(label.to_owned());
            }
            Some("") if current.is_some() && line.len() > LABEL_COLUMN => {
                blocks
                    .get_mut(current.as_deref().unwrap())
                    .unwrap()
                    .push(line[LABEL_COLUMN..].to_owned());
            }
            _ => current = None,
        }
    }
    blocks
        .into_iter()
        .map(|(label, lines)| (label, lines.join("\n")))
        .collect()
}

/// The resolved pair a configuration must carry.
struct Pair<'a> {
    provider: &'a str,
    kind: &'a str,
    args: [&'a str; 4],
    model: &'a str,
    effort: &'a str,
}

/// Each block is a TOML document on its own that yields the resolved pair through the parsers af
/// reads `reviewer.toml`, `worker.toml` and a bindings `[providers]` table with.
fn assert_configuration(reviewer: &str, worker: &str, binding: &str, pair: &Pair) {
    let parsed: toml::Value = toml::from_str(reviewer)
        .unwrap_or_else(|error| panic!("reviewer.toml block: {error}\n{reviewer}"));
    assert_eq!(parsed.as_table().unwrap().len(), 1, "{reviewer}");
    assert_eq!(parsed["runner"]["program"].as_str(), Some(pair.kind));
    let args: Vec<&str> = parsed["runner"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg["value"].as_str().unwrap())
        .collect();
    assert_eq!(args, pair.args, "{reviewer}");
    let runner: review_config::CommandSpec = parsed["runner"].clone().try_into().unwrap();
    let manifest = review_config::lock::PackageManifest {
        name: "x".into(),
        version: "1.0.0".into(),
        subjects: Vec::new(),
        runner,
    };
    let settings = review_config::lock::reviewer_runner_settings_from_manifest(&manifest).unwrap();
    assert_eq!(
        (settings.model.as_str(), settings.effort.as_str()),
        (pair.model, pair.effort)
    );

    let parsed: toml::Value = toml::from_str(worker)
        .unwrap_or_else(|error| panic!("worker.toml block: {error}\n{worker}"));
    assert_eq!(parsed.as_table().unwrap().len(), 1, "{worker}");
    let table = &parsed["runner"];
    assert_eq!(table["kind"].as_str(), Some("model"));
    assert_eq!(table["provider_kind"].as_str(), Some(pair.kind));
    assert_eq!(table["model"].as_str(), Some(pair.model));
    assert_eq!(table["effort"].as_str(), Some(pair.effort));
    let runner: review_config::task::catalog::TaskWorkerRunner = table.clone().try_into().unwrap();
    let review_config::task::catalog::TaskWorkerRunner::Model {
        provider_kind,
        model,
        effort,
    } = runner
    else {
        panic!("not a Model runner: {worker}");
    };
    // The worker.toml parser's own check, then the one validation authority's static rules.
    assert!(review_core::task::is_name(&provider_kind) && review_core::task::is_name(&effort));
    assert!(!model.trim().is_empty());
    let kind = review_runner_claude::models::Kind::parse(&provider_kind).unwrap();
    assert!(review_runner_claude::models::check_static(kind, &model, Some(&effort)).is_ok());
    assert_eq!((model.as_str(), effort.as_str()), (pair.model, pair.effort));

    let parsed: toml::Value = toml::from_str(binding)
        .unwrap_or_else(|error| panic!("bindings block: {error}\n{binding}"));
    assert_eq!(parsed.as_table().unwrap().len(), 1, "{binding}");
    let providers = parsed["providers"].as_table().unwrap();
    assert_eq!(providers.len(), 1, "{binding}");
    assert_eq!(providers["PACKAGE"].as_str(), Some(pair.provider));
    // The review-time equivalent is a comment line of its own, never a trailer on a key.
    let review = format!("# af review run … --provider NODE={}", pair.provider);
    assert!(binding.lines().any(|line| line == review), "{binding}");
    assert!(
        binding
            .lines()
            .filter(|line| line.contains("--provider"))
            .all(|line| line.starts_with('#')),
        "{binding}"
    );
}

/// The printed blocks and the document's `configuration` both carry the pair.
fn assert_both_configurations(text: &str, configuration: &Value, pair: &Pair) {
    let blocks = printed_blocks(text);
    assert_eq!(
        blocks.keys().collect::<Vec<_>>(),
        ["bindings", "reviewer.toml", "worker.toml"],
        "{text}"
    );
    assert_configuration(
        &blocks["reviewer.toml"],
        &blocks["worker.toml"],
        &blocks["bindings"],
        pair,
    );
    let field = |name: &str| configuration[name].as_str().unwrap().to_owned();
    assert_configuration(
        &field("reviewer_runner"),
        &field("worker_runner"),
        &field("binding"),
        pair,
    );
    assert_eq!(field("reviewer_runner"), blocks["reviewer.toml"]);
    assert_eq!(field("worker_runner"), blocks["worker.toml"]);
    assert_eq!(field("binding"), blocks["bindings"]);
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
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    assert!(
        !text.contains("codex-main"),
        "only the named Provider: {text}"
    );
    let fable = text;
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
    assert_both_configurations(
        &fable,
        &resolution["configuration"],
        &Pair {
            provider: "claude-work",
            kind: "claude",
            args: ["--model", "claude-fable-5-1", "--effort", "xhigh"],
            model: "claude-fable-5-1",
            effort: "xhigh",
        },
    );
    for spelling in ["fable", "claude-fable-5.1", "claude-opus-4-8"] {
        let text = resolve(spelling);
        assert!(
            printed_blocks(&text).is_empty(),
            "af never substitutes: {text}"
        );
    }
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
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    let output = machine.af(&["codex-main", "--json", "--resolve", "GPT-6 Sol/ultra"]);
    assert_both_configurations(
        &text,
        &document(&output)["providers"][0]["resolution"]["configuration"],
        &Pair {
            provider: "codex-main",
            kind: "codex",
            args: [
                "--model",
                "gpt-6-sol",
                "-c",
                "model_reasoning_effort=\"ultra\"",
            ],
            model: "gpt-6-sol",
            effort: "ultra",
        },
    );
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
    assert!(
        err(&unknown).contains("Provider nobody not found in this machine's Provider registry")
    );
}
