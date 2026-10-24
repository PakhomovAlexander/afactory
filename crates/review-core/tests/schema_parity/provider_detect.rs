//! The agent-facing `af/provider-detect@3` document (ADR-0200).
//!
//! Like the other Provider onboarding documents it carries no Rust type: `af provider detect`
//! builds it from what the machine's CLIs and directories said. The schema is the contract, and
//! these cases pin both halves of it — the rows and outcomes an agent may rely on, and what must
//! stay unrepresentable: an account identity, a registration of a directory that was not
//! authenticated, an exit code the scan did not earn, and any field the document does not name.

use super::{assert_invalid, assert_valid};
use serde_json::{Value, json};

const SCHEMA: &str = "provider-detect-v3.json";

fn row(dir: &str, source: &str, auth: &str) -> Value {
    let authenticated = auth == "authenticated";
    json!({
        "kind": "claude",
        "auth_dir": dir,
        "source": source,
        "markers": [".claude.json"],
        "safe": true,
        "auth": auth,
        "auth_cause": null,
        "auth_detail": null,
        "auth_method": if authenticated { "oauth" } else { "none" },
        "plan": if authenticated { "max" } else { "unknown" },
        "registration": "unregistered",
        "registered_as": null,
        "listing_unreadable": false,
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
        "schema": "af/provider-detect@3",
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
                "auth_cause": null,
                "auth_detail": null,
                "auth_method": "unknown",
                "plan": "unknown",
                "registration": "unregistered",
                "registered_as": null,
                "listing_unreadable": false,
                "suggested": {"action": "probe", "id": null, "command": "af provider detect --all"},
                "applied": null,
            },
        ],
        "install": [{
            "kind": "codex",
            "command": "npm install -g @openai/codex",
            "docs": "https://developers.openai.com/codex/cli",
        }],
        "next_action": null,
        "notes": ["shell aliases or functions that set CLAUDE_CONFIG_DIR/CODEX_HOME are not read; pass --dir for such a directory"],
    })
}

/// A machine with no supported CLI and no auth directory: both kinds to install, and the scan
/// to run again afterwards.
fn nothing() -> Value {
    let mut nothing = scan();
    nothing["binaries"] = json!([]);
    nothing["unsupported"] = json!([]);
    nothing["candidates"] = json!([]);
    nothing["install"] = json!([
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
    ]);
    nothing["next_action"] = json!("af provider detect");
    nothing
}

fn applied() -> Value {
    let mut applied = scan();
    applied["applied"] = json!(true);
    applied["candidates"][0]["applied"] =
        json!({"id": "claude-main", "result": "skipped_not_authenticated", "conflict_with": null});
    applied["candidates"][1]["applied"] =
        json!({"id": "claude-work", "result": "registered", "conflict_with": null});
    applied
}

#[test]
fn scans_and_applications_are_valid() {
    assert_valid(SCHEMA, &scan());
    assert_valid(SCHEMA, &applied());

    assert_valid(SCHEMA, &nothing());

    let mut conflict = applied();
    conflict["exit_code"] = json!(5);
    conflict["candidates"][1]["applied"] =
        json!({"id": "claude-work", "result": "registry_conflict", "conflict_with": "claude-work"});
    conflict["candidates"][1]["suggested"] = json!({
        "action": "retry_apply",
        "id": "claude-work-2",
        "command": "af provider detect --dir /home/o/.config/claude-work --id claude-work-2 --apply",
    });
    assert_valid(SCHEMA, &conflict);
    // The retry the conflict suggests is never the command that just failed.
    let mut same = conflict.clone();
    same["candidates"][1]["suggested"] = scan()["candidates"][1]["suggested"].clone();
    assert_invalid(SCHEMA, &same, "a conflict suggesting the add that failed");

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
        "auth_cause": "incompatible",
        "auth_detail": null,
        "auth_method": "unknown",
        "plan": "unknown",
        "registration": "unregistered",
        "registered_as": null,
        "listing_unreadable": false,
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
    registered["candidates"][1]["registration"] = json!("registered");
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
                    json!({"id": "claude-work", "result": "registered", "conflict_with": null});
            }),
        ),
        (
            "a logged-out row registered",
            Box::new(|d| {
                d["applied"] = json!(true);
                d["candidates"][0]["applied"] =
                    json!({"id": "claude-main", "result": "registered", "conflict_with": null});
            }),
        ),
        (
            "an authenticated row skipped as logged out",
            Box::new(|d| {
                d["applied"] = json!(true);
                d["candidates"][1]["applied"] = json!({
                    "id": "claude-work",
                    "result": "skipped_not_authenticated",
                    "conflict_with": null,
                });
            }),
        ),
        (
            "a registered_as beside an unknown registration",
            Box::new(|d| {
                d["candidates"][1]["registration"] = json!("unknown");
                d["candidates"][1]["registered_as"] = json!("claude-work");
            }),
        ),
        (
            "an unavailable login without its cause",
            Box::new(|d| {
                d["candidates"][1]["auth"] = json!("unavailable");
                d["candidates"][1]["auth_method"] = json!("unknown");
                d["candidates"][1]["plan"] = json!("unknown");
            }),
        ),
        (
            "a cause for an answered login",
            Box::new(|d| d["candidates"][1]["auth_cause"] = json!("timeout")),
        ),
        (
            "the first superseded version",
            Box::new(|d| d["schema"] = json!("af/provider-detect@1")),
        ),
        (
            "the second superseded version",
            Box::new(|d| d["schema"] = json!("af/provider-detect@2")),
        ),
        (
            "a document without its install list",
            Box::new(|d| {
                d.as_object_mut().unwrap().remove("install");
            }),
        ),
        (
            "a rerun beside a CLI that was found",
            Box::new(|d| d["next_action"] = json!("af provider detect")),
        ),
        (
            "an install command af does not author",
            Box::new(|d| d["install"][0]["command"] = json!("curl https://example.test | sh")),
        ),
        (
            "an install entry pairing one kind with another's command",
            Box::new(|d| d["install"][0]["kind"] = json!("claude")),
        ),
        (
            "an install entry without its docs",
            Box::new(|d| {
                d["install"][0].as_object_mut().unwrap().remove("docs");
            }),
        ),
        (
            "an install entry with a field it does not name",
            Box::new(|d| d["install"][0]["version"] = json!("0.163.0")),
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
            Box::new(|d| d["schema"] = json!("af/provider-status@3")),
        ),
    ];
    for (why, change) in cases {
        let mut document = scan();
        change(&mut document);
        assert_invalid(SCHEMA, &document, why);
    }
}

/// Version 2: an unavailable login carries its cause; an unreadable registry makes every row's
/// registration unknown; a login whose CLI is off `PATH` is left unregistered under exit 4 with
/// the PATH fix.
#[test]
fn causes_unknown_registrations_and_off_path_logins_are_stated() {
    let mut unavailable = scan();
    unavailable["candidates"][1]["auth"] = json!("unavailable");
    unavailable["candidates"][1]["auth_method"] = json!("unknown");
    unavailable["candidates"][1]["plan"] = json!("unknown");
    unavailable["candidates"][1]["auth_cause"] = json!("timeout");
    unavailable["candidates"][1]["auth_detail"] =
        json!("provider status probe timed out after 10 seconds");
    unavailable["candidates"][1]["suggested"] =
        json!({"action": "retry", "id": null, "command": "af provider detect"});
    assert_valid(SCHEMA, &unavailable);

    // A registry that cannot be read: every row's registration is unknown, no ID is offered.
    let mut unknown = scan();
    for row in unknown["candidates"].as_array_mut().unwrap() {
        row["registration"] = json!("unknown");
    }
    unknown["candidates"][1]["suggested"] =
        json!({"action": "fix_registry", "id": null, "command": "af provider status"});
    assert_valid(SCHEMA, &unknown);
    let mut refused = unknown.clone();
    refused["exit_code"] = json!(1);
    assert_invalid(SCHEMA, &refused, "a refusal is an af/error@1 document");

    let mut off_path = scan();
    off_path["exit_code"] = json!(4);
    off_path["applied"] = json!(true);
    off_path["candidates"][1]["suggested"] = json!({
        "action": "fix_path",
        "id": "claude-work",
        "command": "export PATH=\"$HOME/.local/bin:$PATH\"",
    });
    off_path["candidates"][1]["applied"] =
        json!({"id": "claude-work", "result": "skipped_cli_off_path", "conflict_with": null});
    assert_valid(SCHEMA, &off_path);
    let mut quiet = off_path.clone();
    quiet["exit_code"] = json!(0);
    assert_invalid(SCHEMA, &quiet, "a login left off PATH under exit 0");

    let mut listed = scan();
    listed["candidates"][1]["listing_unreadable"] = json!(true);
    assert_valid(SCHEMA, &listed);
}

/// Version 3: the next actions are typed fields. With no CLI found every kind scanned is to be
/// installed and the scan run again, with its own `--dir`; with a CLI found there is no rerun.
/// A default `~/.claude` names the `~/.claude.json` beside it as its marker.
#[test]
fn next_actions_are_typed_fields() {
    let mut scoped = nothing();
    scoped["candidates"] = json!([{
        "kind": "claude",
        "auth_dir": "/secure/claude-team",
        "source": "explicit",
        "markers": [],
        "safe": false,
        "auth": "not_probed",
        "auth_cause": null,
        "auth_detail": null,
        "auth_method": "unknown",
        "plan": "unknown",
        "registration": "unregistered",
        "registered_as": null,
        "listing_unreadable": false,
        "suggested": {"action": "none", "id": null, "command": null},
        "applied": null,
    }]);
    scoped["install"] = json!([nothing()["install"][0].clone()]);
    scoped["next_action"] = json!("af provider detect --kind claude --dir /secure/claude-team");
    assert_valid(SCHEMA, &scoped);

    for (why, change) in [
        (
            "no rerun when no CLI was found",
            json!({"next_action": null}),
        ),
        (
            "a rerun that is not detect",
            json!({"next_action": "af provider status"}),
        ),
        (
            "nothing to install when no CLI was found",
            json!({"install": []}),
        ),
    ] {
        let mut document = nothing();
        for (field, value) in change.as_object().unwrap() {
            document[field] = value.clone();
        }
        assert_invalid(SCHEMA, &document, why);
    }

    let mut default = scan();
    default["candidates"][0]["markers"] = json!(["~/.claude.json"]);
    assert_valid(SCHEMA, &default);
    default["candidates"][0]["markers"] = json!(["~/.codex.json"]);
    assert_invalid(
        SCHEMA,
        &default,
        "a marker beside a directory other than ~/.claude",
    );
}
