//! A Worker node may declare its own Attempt cap. It refines `[budgets]`, never replaces it,
//! and a pipeline without node caps loads exactly as before.

use review_config::Definition;

fn pipeline(alpha_budget: &str, budgets: &str) -> String {
    format!(
        r#"
version = 2
[subject]
kind = "whole-tree"

[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]

[[nodes]]
id = "r-alpha"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = {{ program = "/bin/true" }}
{alpha_budget}

[[nodes]]
id = "r-beta"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = {{ program = "/bin/true" }}

[[nodes]]
id = "gather"
kind = "gather"
inputs = ["r-alpha", "r-beta"]
outputs = ["reports"]

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]

[[edges]]
from = {{ node = "gate", port = "decision" }}
to = {{ node = "r-alpha", port = "gate" }}

[[edges]]
from = {{ node = "gate", port = "decision" }}
to = {{ node = "r-beta", port = "gate" }}

[[edges]]
from = {{ node = "r-alpha", port = "result" }}
to = {{ node = "gather", port = "r-alpha" }}

[[edges]]
from = {{ node = "r-beta", port = "result" }}
to = {{ node = "gather", port = "r-beta" }}

[[edges]]
from = {{ node = "gather", port = "reports" }}
to = {{ node = "ledger", port = "reports" }}

{budgets}
"#
    )
}

const BUDGETS: &str = "[budgets]\nunit = \"tokens\"\nattempt = 100000\nrun = 250000\n";

#[test]
fn a_node_cap_is_loaded_and_the_others_keep_the_pipeline_cap() {
    let loaded = Definition::from_toml(&pipeline("budget = { attempt = 50000 }", BUDGETS))
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(loaded.node_attempt_caps().get("r-alpha"), Some(&50_000));
    assert_eq!(loaded.node_attempt_caps().get("r-beta"), None);
    assert_eq!(loaded.attempt_cap_for("r-alpha"), Some(50_000));
    assert_eq!(loaded.attempt_cap_for("r-beta"), Some(100_000));
}

#[test]
fn a_pipeline_without_node_caps_is_unchanged() {
    let loaded = Definition::from_toml(&pipeline("", BUDGETS))
        .unwrap()
        .load()
        .unwrap();
    assert!(loaded.node_attempt_caps().is_empty());
    assert_eq!(loaded.attempt_cap_for("r-alpha"), Some(100_000));
    let uncapped = Definition::from_toml(&pipeline("", ""))
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(uncapped.attempt_cap_for("r-alpha"), None);
}

#[test]
fn a_node_cap_must_refine_real_pipeline_caps() {
    for (alpha_budget, budgets, expected) in [
        ("budget = { attempt = 50000 }", "", "has no [budgets]"),
        (
            "budget = { attempt = 0 }",
            BUDGETS,
            "zero-token Attempt cap",
        ),
        (
            "budget = { attempt = 250001 }",
            BUDGETS,
            "exceeds the run cap",
        ),
    ] {
        let error = Definition::from_toml(&pipeline(alpha_budget, budgets))
            .unwrap()
            .load()
            .err()
            .expect("the pipeline must be refused")
            .to_string();
        assert!(error.contains(expected), "{alpha_budget}: {error}");
    }
    // Only Workers reserve, so only Workers may cap.
    let gate_capped = pipeline("", BUDGETS).replace(
        "id = \"gate\"\nkind = \"gate\"\n",
        "id = \"gate\"\nkind = \"gate\"\nbudget = { attempt = 1 }\n",
    );
    let error = Definition::from_toml(&gate_capped)
        .unwrap()
        .load()
        .err()
        .expect("the pipeline must be refused")
        .to_string();
    assert!(error.contains("not a Worker"), "{error}");
}
