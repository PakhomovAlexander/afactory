//! The two agent-facing Provider onboarding documents.
//!
//! They carry no Rust type: `af provider status` and `af provider setup` build them from
//! machine-local state that no kernel artifact owns. The schema is therefore the contract, and
//! these cases pin both halves of it — the states an agent may rely on, and the personal,
//! credential, OAuth and raw-Provider fields that must stay unrepresentable.

use super::{assert_invalid, assert_valid};
use serde_json::{Value, json};

fn status() -> Value {
    json!({
        "schema": "af/provider-status@1",
        "exit_code": 0,
        "usage_requested": false,
        "registry": {"path": "/home/o/.config/af/providers.toml", "warning": null},
        "providers": [{
            "id": "codex-main",
            "kind": "codex",
            "registered": true,
            "auth_context": "/home/o/.codex",
            "auth": "authenticated",
            "credential": "subscription",
            "usability": "usable_or_untested",
            "usage": {"state": "not_requested", "windows": []},
        }],
    })
}

fn setup() -> Value {
    json!({
        "schema": "af/provider-setup@1",
        "result": "registered",
        "exit_code": 0,
        "login_launched": false,
        "provider": {
            "id": "codex-main",
            "kind": "codex",
            "auth_context": "/home/o/.codex",
            "registered": true,
            "auth": "authenticated",
            "usability": "usable_or_untested",
        },
        "next_action": null,
        "diagnostic": null,
    })
}

#[test]
fn status_states_are_stable_and_independent() {
    assert_valid("provider-status-v1.json", &status());

    let window = json!({
        "window_minutes": 300,
        "used_percent": 42,
        "resets_at_unix": 1_800_000_000_u64,
    });
    let mut probed = status();
    probed["usage_requested"] = json!(true);
    probed["providers"][0]["usage"] = json!({"state": "available", "windows": [window]});
    assert_valid("provider-status-v1.json", &probed);

    // The whole point of the usage axis: a probe that did not answer leaves authentication and
    // usability exactly where they were.
    let mut unavailable = probed.clone();
    unavailable["exit_code"] = json!(7);
    unavailable["providers"][0]["usage"] = json!({"state": "unavailable", "windows": []});
    assert_valid("provider-status-v1.json", &unavailable);
    assert_eq!(
        unavailable["providers"][0]["auth"],
        json!("authenticated"),
        "an unavailable usage probe must not touch authentication"
    );

    let mut unsupported = probed.clone();
    unsupported["providers"][0]["credential"] = json!("api_key");
    unsupported["providers"][0]["usage"] = json!({"state": "unsupported", "windows": []});
    assert_valid("provider-status-v1.json", &unsupported);

    let mut ambient = status();
    ambient["providers"][0] = json!({
        "id": "codex-ambient",
        "kind": "codex",
        "registered": false,
        "auth_context": null,
        "auth": "not_authenticated",
        "credential": "none",
        "usability": "unusable",
        "usage": {"state": "not_applicable", "windows": []},
    });
    assert_valid("provider-status-v1.json", &ambient);

    for (path, replacement, why) in [
        ("/exit_code", json!(1), "undocumented status exit code"),
        (
            "/providers/0/usability",
            json!("usable"),
            "status cannot prove usability",
        ),
        ("/providers/0/auth", json!("logged in"), "open auth state"),
        ("/providers/0/kind", json!("gemini"), "unknown kind"),
        (
            "/providers/0/credential",
            json!("oauth-token"),
            "credential material in the credential axis",
        ),
        (
            "/schema",
            json!("af/provider-status@2"),
            "unversioned drift",
        ),
    ] {
        let mut invalid = status();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("provider-status-v1.json", &invalid, why);
    }

    // Exit 7 is a claim about a requested probe, never about a skipped one.
    let mut unrequested = status();
    unrequested["exit_code"] = json!(7);
    assert_invalid(
        "provider-status-v1.json",
        &unrequested,
        "exit 7 without a usage request",
    );

    // Windows exist only where a probe answered, so a skipped probe cannot smuggle numbers in.
    let mut smuggled = status();
    smuggled["providers"][0]["usage"]["windows"] =
        json!([{"window_minutes": 300, "used_percent": 42, "resets_at_unix": null}]);
    assert_invalid(
        "provider-status-v1.json",
        &smuggled,
        "windows without an answered probe",
    );

    // An authenticated Provider may never be reported as unusable by a free status check.
    let mut demoted = status();
    demoted["providers"][0]["usability"] = json!("unusable");
    assert_invalid(
        "provider-status-v1.json",
        &demoted,
        "authenticated but unusable",
    );
}

#[test]
fn status_never_carries_personal_credential_or_provider_text() {
    for key in [
        "email",
        "account_email",
        "organization_id",
        "account_id",
        "oauth_url",
        "authorization_code",
        "api_key",
        "token",
        "provider_stdout",
    ] {
        let mut leaked = status();
        leaked["providers"][0][key] = json!("leaked");
        assert_invalid("provider-status-v1.json", &leaked, key);
        let mut leaked_root = status();
        leaked_root[key] = json!("leaked");
        assert_invalid("provider-status-v1.json", &leaked_root, key);
    }
    let mut labelled = status();
    labelled["usage_requested"] = json!(true);
    labelled["providers"][0]["usage"] = json!({
        "state": "available",
        "windows": [{
            "window_minutes": 300,
            "used_percent": 42,
            "resets_at_unix": null,
            "name": "Codex weekly (gpt-5-codex)",
        }],
    });
    assert_invalid(
        "provider-status-v1.json",
        &labelled,
        "Provider-authored window label",
    );
}

#[test]
fn setup_outcomes_bind_result_exit_code_and_next_action() {
    assert_valid("provider-setup-v1.json", &setup());

    let mut already = setup();
    already["result"] = json!("already_registered");
    assert_valid("provider-setup-v1.json", &already);

    // The safe non-interactive outcome: no login was started, and a human is given the exact
    // command instead of an OAuth exchange.
    let mut human = setup();
    human["result"] = json!("human_action_required");
    human["exit_code"] = json!(3);
    human["provider"]["registered"] = json!(false);
    human["provider"]["auth"] = json!("not_authenticated");
    human["provider"]["usability"] = json!("unusable");
    human["next_action"] = json!({
        "kind": "private_terminal",
        "command": "af provider setup codex-main --kind codex --auth-dir /home/o/.codex --login",
    });
    human["diagnostic"] = json!("af refuses to start the codex CLI login outside a terminal");
    assert_valid("provider-setup-v1.json", &human);
    let mut opt_in = human.clone();
    opt_in["next_action"]["kind"] = json!("interactive_login");
    assert_valid("provider-setup-v1.json", &opt_in);

    for (result, code, auth) in [
        ("provider_cli_missing", 4, "unknown"),
        ("registry_conflict", 5, "not_probed"),
        ("authentication_failed", 6, "not_authenticated"),
    ] {
        let mut failed = setup();
        failed["result"] = json!(result);
        failed["exit_code"] = json!(code);
        failed["provider"]["registered"] = json!(false);
        failed["provider"]["auth"] = json!(auth);
        let usability = if auth == "not_authenticated" {
            "unusable"
        } else {
            "unknown"
        };
        failed["provider"]["usability"] = json!(usability);
        failed["diagnostic"] = json!("af-authored reason");
        assert_valid("provider-setup-v1.json", &failed);

        let mut mismatched = failed.clone();
        mismatched["exit_code"] = json!(0);
        assert_invalid("provider-setup-v1.json", &mismatched, "exit code drift");
    }

    for (path, replacement, why) in [
        ("/login_launched", json!(true), "never after a login"),
        ("/result", json!("logged_in"), "open result vocabulary"),
        ("/exit_code", json!(1), "unclassified exit code"),
        (
            "/next_action",
            json!({"kind": "interactive_login", "command": "curl https://example.test"}),
            "next action that is not an af command",
        ),
        ("/provider/auth", json!("unavailable"), "open auth state"),
        ("/schema", json!("af/provider-setup@2"), "unversioned drift"),
    ] {
        let mut invalid = setup();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert_invalid("provider-setup-v1.json", &invalid, why);
    }

    // Success may not carry a next action, and a refusal may not claim registration.
    let mut succeeded_with_action = setup();
    succeeded_with_action["next_action"] = json!({
        "kind": "interactive_login",
        "command": "af provider setup codex-main --kind codex --login",
    });
    assert_invalid(
        "provider-setup-v1.json",
        &succeeded_with_action,
        "success with a pending human action",
    );
    let mut refusal_claiming_success = setup();
    refusal_claiming_success["result"] = json!("registry_conflict");
    refusal_claiming_success["exit_code"] = json!(5);
    refusal_claiming_success["diagnostic"] = json!("provider `codex-main` already exists");
    assert_invalid(
        "provider-setup-v1.json",
        &refusal_claiming_success,
        "refusal that still claims an authenticated registration",
    );
}

#[test]
fn setup_never_carries_oauth_credential_or_provider_output() {
    for key in [
        "oauth_url",
        "verification_uri",
        "authorization_code",
        "device_code",
        "access_token",
        "refresh_token",
        "api_key",
        "email",
        "organization_id",
        "provider_stdout",
        "provider_stderr",
    ] {
        let mut leaked = setup();
        leaked[key] = json!("leaked");
        assert_invalid("provider-setup-v1.json", &leaked, key);
        let mut leaked_provider = setup();
        leaked_provider["provider"][key] = json!("leaked");
        assert_invalid("provider-setup-v1.json", &leaked_provider, key);
    }
    let mut leaked_action = setup();
    leaked_action["result"] = json!("human_action_required");
    leaked_action["exit_code"] = json!(3);
    leaked_action["provider"]["registered"] = json!(false);
    leaked_action["provider"]["auth"] = json!("not_authenticated");
    leaked_action["provider"]["usability"] = json!("unusable");
    leaked_action["diagnostic"] = json!("login required");
    leaked_action["next_action"] = json!({
        "kind": "interactive_login",
        "command": "af provider setup codex-main --kind codex --login",
        "oauth_url": "https://auth.example.test/device?code=ABCD-EFGH",
    });
    assert_invalid(
        "provider-setup-v1.json",
        &leaked_action,
        "OAuth material beside the next action",
    );
}
