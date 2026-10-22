//! The agent-facing `af/provider-detect@1` document (ADR-0196).
//!
//! Like the other Provider onboarding documents it carries no Rust type: `af provider detect`
//! builds it from what the machine's CLIs and directories said. The schema is the contract, and
//! these cases pin both halves of it — the rows and outcomes an agent may rely on, and what must
//! stay unrepresentable: an account identity, a registration of a directory that was not
//! authenticated, an exit code the scan did not earn, and any field the document does not name.

use super::{assert_invalid, assert_valid};
use serde_json::{Value, json};

const SCHEMA: &str = "provider-detect-v1.json";

fn row(dir: &str, source: &str, auth: &str) -> Value {
    let authenticated = auth == "authenticated";
    json!({
        "kind": "claude",
        "auth_dir": dir,
        "source": source,
        "markers": [".claude.json"],
        "safe": true,
        "auth": auth,
        "auth_method": if authenticated { "oauth" } else { "none" },
        "plan": if authenticated { "max" } else { "unknown" },
        "registered_as": null,
        "suggested": if authenticated {
            json!({
                "action": "register",
                "id": "claude-work",
                "command": format!("af provider add claude-work --kind claude --auth-dir {dir}"),
            })
        } else {
            json!({
                "action": "login",
                "id": "claude-main",
                "command": format!("af provider setup claude-main --kind claude --auth-dir {dir} --login"),
            })
        },
        "applied": null,
    })
}

fn scan() -> Value {
    json!({
        "schema": "af/provider-detect@1",
        "exit_code": 0,
        "applied": false,
        "binaries": [{
            "kind": "claude",
            "path": "/home/o/.local/bin/claude",
            "version": "2.1.299",
            "on_path": false,
            "compatible": true,
        }],
        "unsupported": [{"name": "gemini", "path": "/usr/bin/gemini", "version": null}],
        "candidates": [
            row("/home/o/.claude", "default", "not_authenticated"),
            row("/home/o/.config/claude-work", "sibling", "authenticated"),
            {
                "kind": "codex",
                "auth_dir": "/home/o/.codex-new",
                "source": "sibling",
                "markers": [],
                "safe": true,
                "auth": "not_probed",
                "auth_method": "unknown",
                "plan": "unknown",
                "registered_as": null,
                "suggested": {"action": "probe", "id": null, "command": "af provider detect --all"},
                "applied": null,
            },
        ],
        "notes": ["shell aliases or functions that set CLAUDE_CONFIG_DIR/CODEX_HOME are not read; pass --dir for such a directory"],
    })
}

fn applied() -> Value {
    let mut applied = scan();
    applied["applied"] = json!(true);
    applied["candidates"][0]["applied"] =
        json!({"id": "claude-main", "result": "skipped_not_authenticated"});
    applied["candidates"][1]["applied"] = json!({"id": "claude-work", "result": "registered"});
    applied
}

#[test]
fn scans_and_applications_are_valid() {
    assert_valid(SCHEMA, &scan());
    assert_valid(SCHEMA, &applied());

    let mut nothing = scan();
    nothing["binaries"] = json!([]);
    nothing["unsupported"] = json!([]);
    nothing["candidates"] = json!([]);
    assert_valid(SCHEMA, &nothing);

    let mut conflict = applied();
    conflict["exit_code"] = json!(5);
    conflict["candidates"][1]["applied"] =
        json!({"id": "claude-work", "result": "registry_conflict"});
    assert_valid(SCHEMA, &conflict);

    let mut broken = scan();
    broken["exit_code"] = json!(4);
    broken["binaries"][0]["compatible"] = json!(false);
    broken["candidates"][1] = json!({
        "kind": "claude",
        "auth_dir": "/home/o/.config/claude-work",
        "source": "sibling",
        "markers": ["settings.json"],
        "safe": true,
        "auth": "incompatible",
        "auth_method": "unknown",
        "plan": "unknown",
        "registered_as": null,
        "suggested": {
            "action": "fix_cli",
            "id": null,
            "command": "npm install -g @anthropic-ai/claude-code@latest",
        },
        "applied": null,
    });
    assert_valid(SCHEMA, &broken);

    let mut registered = scan();
    registered["candidates"][1]["source"] = json!("registry");
    registered["candidates"][1]["registered_as"] = json!("claude-work");
    registered["candidates"][1]["suggested"] =
        json!({"action": "none", "id": null, "command": null});
    assert_valid(SCHEMA, &registered);

    let mut unsafe_row = scan();
    unsafe_row["candidates"][2]["safe"] = json!(false);
    unsafe_row["candidates"][2]["suggested"] = json!({
        "action": "fix_safety",
        "id": null,
        "command": "chmod go-w -- /home/o/.codex-new",
    });
    assert_valid(SCHEMA, &unsafe_row);
}

#[test]
fn identities_raw_text_and_unearned_outcomes_are_unrepresentable() {
    for (path, value, why) in [
        (
            "/candidates/1/email",
            json!("person@example.test"),
            "account email",
        ),
        (
            "/candidates/1/organization",
            json!("org_SECRET"),
            "organization",
        ),
        (
            "/binaries/0/version_line",
            json!("2.1.299 (Claude Code)"),
            "raw CLI text",
        ),
        (
            "/detail",
            json!("Error: token=sk-ant"),
            "Provider-authored text",
        ),
    ] {
        let mut document = scan();
        let (parent, field) = path.rsplit_once('/').unwrap();
        document.pointer_mut(parent).unwrap()[field] = value;
        assert_invalid(SCHEMA, &document, why);
    }
    type Change = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Change)> = vec![
        (
            "a CLI line as a version",
            Box::new(|d| d["binaries"][0]["version"] = json!("2.1.299 (Claude Code)")),
        ),
        (
            "an account as the login label",
            Box::new(|d| d["candidates"][1]["plan"] = json!("person@example.test")),
        ),
        (
            "an exit code outside 0/4/5",
            Box::new(|d| d["exit_code"] = json!(6)),
        ),
        ("exit 3", Box::new(|d| d["exit_code"] = json!(3))),
        (
            "exit 4 without a broken CLI",
            Box::new(|d| d["exit_code"] = json!(4)),
        ),
        (
            "a broken CLI under exit 0",
            Box::new(|d| d["binaries"][0]["compatible"] = json!(false)),
        ),
        (
            "exit 5 without --apply",
            Box::new(|d| d["exit_code"] = json!(5)),
        ),
        (
            "an outcome without --apply",
            Box::new(|d| {
                d["candidates"][1]["applied"] =
                    json!({"id": "claude-work", "result": "registered"});
            }),
        ),
        (
            "a logged-out row registered",
            Box::new(|d| {
                d["applied"] = json!(true);
                d["candidates"][0]["applied"] =
                    json!({"id": "claude-main", "result": "registered"});
            }),
        ),
        (
            "an authenticated row skipped as logged out",
            Box::new(|d| {
                d["applied"] = json!(true);
                d["candidates"][1]["applied"] =
                    json!({"id": "claude-work", "result": "skipped_not_authenticated"});
            }),
        ),
        (
            "an authenticated unregistered row not offered",
            Box::new(|d| {
                d["candidates"][1]["suggested"] =
                    json!({"action": "none", "id": null, "command": null});
            }),
        ),
        (
            "an unsafe row probed",
            Box::new(|d| d["candidates"][1]["safe"] = json!(false)),
        ),
        (
            "a plan for a row without a login",
            Box::new(|d| d["candidates"][0]["plan"] = json!("max")),
        ),
        (
            "a register line that is not add",
            Box::new(|d| {
                d["candidates"][1]["suggested"]["command"] = json!("claude auth login");
            }),
        ),
        (
            "a login line without --login",
            Box::new(|d| {
                d["candidates"][0]["suggested"]["command"] =
                    json!("af provider setup claude-main --kind claude");
            }),
        ),
        (
            "an id on a probe suggestion",
            Box::new(|d| {
                d["candidates"][2]["suggested"]["id"] = json!("codex-new");
            }),
        ),
        (
            "an unsafe id",
            Box::new(|d| d["candidates"][1]["suggested"]["id"] = json!("../x")),
        ),
        (
            "an unknown source",
            Box::new(|d| d["candidates"][1]["source"] = json!("shell_rc")),
        ),
        (
            "an unknown marker",
            Box::new(|d| d["candidates"][1]["markers"] = json!(["history.jsonl"])),
        ),
        (
            "a relative auth_dir",
            Box::new(|d| d["candidates"][1]["auth_dir"] = json!("claude-work")),
        ),
        (
            "an unsupported CLI outside the fixed list",
            Box::new(|d| d["unsupported"][0]["name"] = json!("my-agent")),
        ),
        (
            "another document's schema",
            Box::new(|d| d["schema"] = json!("af/provider-status@2")),
        ),
    ];
    for (why, change) in cases {
        let mut document = scan();
        change(&mut document);
        assert_invalid(SCHEMA, &document, why);
    }
}
