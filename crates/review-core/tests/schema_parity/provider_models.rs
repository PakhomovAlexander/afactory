//! The agent-facing `af/provider-models@1` document (ADR-0148).
//!
//! Like the onboarding documents it carries no Rust type: `af provider models` builds it from
//! what each harness's own protocol answered. The schema is the contract, and these cases pin
//! both halves of it — the states and rows an agent may rely on, and what must stay
//! unrepresentable: a listed row for a Provider that was not listed, a configuration for a pair af
//! would have to substitute, and any spelling outside af's printable grammar.

use super::{assert_invalid, assert_valid};
use serde_json::{Value, json};

const SCHEMA: &str = "provider-models-v1.json";

fn fable() -> Value {
    json!({
        "id": "claude-fable-5-1",
        "display_name": "Fable 5.1",
        "aliases": ["fable"],
        "effort_levels": ["low", "medium", "high", "xhigh", "max"],
        "default_effort": null,
        "hidden": false,
    })
}

fn listed() -> Value {
    json!({
        "schema": "af/provider-models@1",
        "exit_code": 0,
        "token_free": true,
        "proves": "af recognises these spellings for this Provider",
        "providers": [{
            "id": "claude-work",
            "kind": "claude",
            "state": "listed",
            "source": "claude list_models",
            "harness_version": "2.1.300",
            "freshness": {"source": "live", "fetched_at": null},
            "models": [fable()],
            "skipped_entries": 0,
            "static_rules": {
                "effort_levels": ["low", "medium", "high", "xhigh", "max"],
                "documented_aliases": [{"alias": "fable", "id": "claude-fable-5-1"}],
            },
            "resolution": null,
        }],
    })
}

fn resolved() -> Value {
    json!({
        "spelling": "Fable 5.1",
        "effort": "xhigh",
        "outcome": "display_name",
        "id": "claude-fable-5-1",
        "nearest": [],
        "spelling_rule": null,
        "effort_check": "listed",
        "effort_levels": ["low", "medium", "high", "xhigh", "max"],
        "configuration": {
            "reviewer_runner": "[runner] program = \"claude\"",
            "worker_runner": "[runner]\nkind = \"model\"",
            "binding": "[providers] \"PACKAGE\" = \"claude-work\"",
        },
    })
}

#[test]
fn listed_unavailable_and_resolved_documents_are_valid() {
    assert_valid(SCHEMA, &listed());

    let mut codex = listed();
    codex["providers"][0]["kind"] = json!("codex");
    codex["providers"][0]["source"] = json!("codex app-server model/list");
    codex["providers"][0]["freshness"] =
        json!({"source": "models_cache", "fetched_at": "2026-10-15T09:58:12Z"});
    codex["providers"][0]["models"][0]["id"] = json!("gpt-6.1-sol");
    codex["providers"][0]["models"][0]["default_effort"] = json!("low");
    assert_valid(SCHEMA, &codex);

    let mut unavailable = listed();
    unavailable["exit_code"] = json!(7);
    unavailable["providers"][0]["state"] = json!("unavailable");
    unavailable["providers"][0]["harness_version"] = Value::Null;
    unavailable["providers"][0]["models"] = json!([]);
    assert_valid(SCHEMA, &unavailable);

    let mut resolution = listed();
    resolution["providers"][0]["resolution"] = resolved();
    assert_valid(SCHEMA, &resolution);

    let mut not_listed = listed();
    let mut outcome = resolved();
    outcome["spelling"] = json!("claude-opus-4-8");
    outcome["outcome"] = json!("not_listed");
    outcome["id"] = Value::Null;
    outcome["spelling_rule"] = json!("passes");
    outcome["effort_check"] = json!("in_vocabulary");
    outcome["effort_levels"] = json!([]);
    outcome["configuration"] = Value::Null;
    not_listed["providers"][0]["resolution"] = outcome;
    assert_valid(SCHEMA, &not_listed);
}

#[test]
fn what_af_never_reports_is_unrepresentable() {
    let mut rows = listed();
    rows["providers"][0]["state"] = json!("unavailable");
    rows["providers"][0]["harness_version"] = Value::Null;
    assert_invalid(SCHEMA, &rows, "rows for a catalog that was not listed");

    let mut path = listed();
    path["providers"][0]["models"][0]["id"] = json!("/Users/alice/models/fable");
    assert_invalid(SCHEMA, &path, "a model ID outside the printable grammar");

    let mut account = listed();
    account["providers"][0]["email"] = json!("developer@example.test");
    assert_invalid(SCHEMA, &account, "an account identity");

    let mut substituted = listed();
    let mut outcome = resolved();
    outcome["effort_check"] = json!("not_listed");
    substituted["providers"][0]["resolution"] = outcome;
    assert_invalid(
        SCHEMA,
        &substituted,
        "a configuration for an effort level the model does not list",
    );

    let mut alias_bound = listed();
    let mut outcome = resolved();
    outcome["outcome"] = json!("misspelled");
    alias_bound["providers"][0]["resolution"] = outcome;
    assert_invalid(SCHEMA, &alias_bound, "an exact ID for a misspelling");

    let mut charged = listed();
    charged["token_free"] = json!(false);
    assert_invalid(SCHEMA, &charged, "a charged catalog read");

    let mut code = listed();
    code["exit_code"] = json!(3);
    assert_invalid(SCHEMA, &code, "an exit code outside 0, 4 and 7");
}
