//! Loading a pipeline definition, and refusing a broken one.
//!
//! Every rejection here happens before a single node runs. A pipeline that is 90% valid is not
//! 90% of a review, so there is no partial load and no defaulting-around-a-typo.

use review_config::{ConfigError, Definition};
use review_core::SubjectKind;
use review_graph::PlanError;

fn workspace_root() -> std::path::PathBuf {
    std::env::var_os("AFACTORY_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

const MINIMAL: &str = r#"
version = 2

[subject]
kind = "whole-tree"

[[checks]]
name = "build"
program = "/bin/sh"
args = [{ value = "./build.sh" }]

[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]

[[nodes]]
id = "architecture"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/sh", args = [{ value = "-c" }, { value = "echo hi" }] }

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]

[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "architecture", port = "gate" }

[[edges]]
from = { node = "architecture", port = "result" }
to = { node = "ledger", port = "reports" }
"#;

const DYNAMIC_V5: &str = r#"
version = 5

[subject]
kind = "whole-tree"

[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"

[budgets]
unit = "tokens"
attempt = 100
fan_out = 200
run = 400

[[nodes]]
id = "gate"
kind = "gate"
outputs = [{ name = "decision", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]

[[nodes]]
id = "slicer"
kind = "slicer"
outputs = [{ name = "slices", type = "review.kernel/SliceSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
slicing = { scatter = "scatter", max_paths_per_slice = 1, max_fanout = 2, coverage = "complete", all_shards_required = true, closeout = "required" }

[[nodes]]
id = "scatter"
kind = "scatter"
inputs = [
  { name = "slices", type = "review.kernel/SliceSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
outputs = [{ name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
runner = { program = "/bin/true" }
execution = { credential_mode = "credential_free" }

[[nodes]]
id = "closeout"
kind = "reviewer"
closeout_for = "scatter"
inputs = [
  { name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
runner = { program = "/bin/true" }
execution = { credential_mode = "credential_free" }

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [
  { name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "closeout", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "scatter", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "closeout", port = "prior_findings" }
[[edges]]
from = { node = "slicer", port = "slices" }
to = { node = "scatter", port = "slices" }
[[edges]]
from = { node = "scatter", port = "shards" }
to = { node = "closeout", port = "shards" }
[[edges]]
from = { node = "scatter", port = "shards" }
to = { node = "ledger", port = "shards" }
[[edges]]
from = { node = "closeout", port = "result" }
to = { node = "ledger", port = "closeout" }
"#;

#[test]
fn a_definition_loads_into_a_plan_with_bindings() {
    let loaded = Definition::from_toml(MINIMAL).unwrap().load().unwrap();

    assert_eq!(loaded.plan_order(), ["gate", "architecture", "ledger"]);
    assert_eq!(loaded.checks().len(), 1);
    assert!(
        loaded.checks()[0].required,
        "checks are required by default"
    );
    assert_eq!(loaded.reviewers().len(), 1);
    assert!(loaded.reviewers().contains_key("architecture"));
    assert_eq!(
        loaded.demand_requirements()["architecture"],
        review_core::DemandRequirement::Required,
        "old pinned pipelines retain the permanent required default"
    );
    assert_eq!(loaded.convergence().max_rounds, 3);
    assert_eq!(loaded.convergence().gate, review_core::Severity::Major);
    assert_eq!(loaded.subject_kind(), SubjectKind::WholeTree);
    assert_eq!(loaded.check_timeout_seconds(), 3600);
}

#[test]
fn version_five_captures_bounded_scatter_and_requires_closeout() {
    let loaded = Definition::from_toml(DYNAMIC_V5).unwrap().load().unwrap();
    assert_eq!(loaded.slicing()["slicer"].scatter, "scatter");
    assert_eq!(loaded.closeouts()["scatter"], "closeout");
    assert_eq!(loaded.budgets().unwrap().fan_out, Some(200));
    assert!(loaded.reviewers().contains_key("scatter"));

    let missing = DYNAMIC_V5.replace("closeout_for = \"scatter\"\n", "");
    assert!(matches!(
        Definition::from_toml(&missing).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("closeout")
    ));
}

#[test]
fn version_five_captures_closed_automatic_integration_policy() {
    let configured = DYNAMIC_V5.replace(
        "[budgets]\n",
        "[[checks]]\nname = \"postapply\"\nprogram = \"/usr/bin/true\"\n\n[integration]\nprotected_paths = [\".github\"]\npost_apply_checks = [\"postapply\"]\nreviewer_priority = [\"scatter\"]\n\n[budgets]\n",
    );
    let loaded = Definition::from_toml(&configured).unwrap().load().unwrap();
    let integration = loaded.integration().unwrap();
    assert_eq!(integration.post_apply_checks, ["postapply"]);
    assert_eq!(integration.reviewer_priority, ["scatter"]);

    let unknown = configured.replace("[\"postapply\"]", "[\"missing\"]");
    assert!(matches!(
        Definition::from_toml(&unknown).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("post_apply_checks")
    ));
    let unsafe_path = configured.replace("[\".github\"]", "[\"../outside\"]");
    assert!(matches!(
        Definition::from_toml(&unsafe_path).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("protected_paths")
    ));

    let static_pipeline = r#"
version = 5
[subject]
kind = "whole-tree"
[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"
[integration]
post_apply_checks = ["postapply"]
[[checks]]
name = "postapply"
program = "/usr/bin/true"
[[nodes]]
id = "gate"
kind = "gate"
outputs = [{ name = "decision", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "correctness"
kind = "reviewer"
inputs = [
  { name = "gate", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
runner = { program = "/bin/true" }
execution = { credential_mode = "credential_free", auto_apply = true }
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "correctness", port = "gate" }
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "correctness", port = "prior_findings" }
[[edges]]
from = { node = "correctness", port = "result" }
to = { node = "ledger", port = "result" }
[convergence]
clean_rounds = 1
max_rounds = 2
gate = "major"
"#;
    assert!(matches!(
        Definition::from_toml(static_pipeline).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("semantic-closure route")
    ));
}

#[test]
fn reviewer_demand_classification_is_pipeline_owned() {
    let advisory = MINIMAL.replace(
        "id = \"architecture\"\nkind = \"reviewer\"\n",
        "id = \"architecture\"\nkind = \"reviewer\"\ndemands = \"advisory\"\n",
    );
    let loaded = Definition::from_toml(&advisory).unwrap().load().unwrap();
    assert_eq!(
        loaded.demand_requirements()["architecture"],
        review_core::DemandRequirement::Advisory
    );

    let invalid = MINIMAL.replace(
        "id = \"gate\"\nkind = \"gate\"\n",
        "id = \"gate\"\nkind = \"gate\"\ndemands = \"advisory\"\n",
    );
    assert!(matches!(
        Definition::from_toml(&invalid).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("not a reviewer")
    ));
}

#[test]
fn check_timeout_is_validated_and_resolved_from_pipeline_authority() {
    let configured = MINIMAL.replace("version = 2", "version = 2\ncheck_timeout_seconds = 17");
    assert_eq!(
        Definition::from_toml(&configured)
            .unwrap()
            .load()
            .unwrap()
            .check_timeout_seconds(),
        17
    );

    let zero = MINIMAL.replace("version = 2", "version = 2\ncheck_timeout_seconds = 0");
    assert!(matches!(
        Definition::from_toml(&zero).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("check_timeout_seconds")
    ));
}

/// Provenance defaults to `literal`, because the project writing its own command is trusted.
/// The unsafe classification is the one that must be typed out.
#[test]
fn an_untrusted_argument_must_say_so() {
    let loaded = Definition::from_toml(MINIMAL).unwrap().load().unwrap();
    let command = &loaded.reviewers()["architecture"];
    assert!(
        command
            .args
            .iter()
            .all(|a| a.provenance == review_core::Provenance::Literal),
        "unmarked arguments are literals"
    );

    let with_untrusted = MINIMAL.replace(
        r#"{ value = "echo hi" }"#,
        r#"{ value = "--config=/tmp/evil", provenance = "untrusted" }"#,
    );
    let loaded = Definition::from_toml(&with_untrusted)
        .unwrap()
        .load()
        .unwrap();
    // Loading succeeds — the refusal happens at execution, where the value is known to be an
    // option position. The definition is allowed to *describe* an untrusted slot.
    assert!(loaded.reviewers()["architecture"].resolve().is_err());
}

#[test]
fn a_typo_in_a_field_is_an_error_not_a_silent_default() {
    let typo = MINIMAL.replace("gated_by = \"gate\"", "gated_bye = \"gate\"");
    match Definition::from_toml(&typo) {
        Err(ConfigError::Parse(message)) => assert!(
            message.contains("gated_bye"),
            "the error should name the typo: {message}"
        ),
        other => panic!("a typo must be refused, got {other:?}"),
    }
}

#[test]
fn a_reviewer_with_no_runner_is_refused() {
    let unbound = MINIMAL.replace(
        r#"runner = { program = "/bin/sh", args = [{ value = "-c" }, { value = "echo hi" }] }"#,
        "",
    );
    match Definition::from_toml(&unbound).unwrap().load() {
        Err(ConfigError::Binding(message)) => {
            assert!(message.contains("architecture"), "{message}");
            assert!(message.contains("always reports nothing"), "{message}");
        }
        Err(other) => panic!("expected a binding error, got {other}"),
        Ok(_) => panic!("an unbound reviewer must be refused"),
    }
}

#[test]
fn a_non_reviewer_with_a_runner_is_refused() {
    let confused = MINIMAL.replace(
        "id = \"ledger\"\nkind = \"ledger\"",
        "id = \"ledger\"\nkind = \"ledger\"\nrunner = { program = \"/bin/sh\" }",
    );
    assert!(matches!(
        Definition::from_toml(&confused).unwrap().load(),
        Err(ConfigError::Binding(_))
    ));
}

/// The graph's own validation reaches through the config unchanged — an edge to a port that does
/// not exist is caught here, not at run time.
#[test]
fn graph_validation_applies_to_definitions_too() {
    let bad_port = MINIMAL.replace(
        r#"to = { node = "architecture", port = "gate" }"#,
        r#"to = { node = "architecture", port = "gates" }"#,
    );
    assert!(matches!(
        Definition::from_toml(&bad_port).unwrap().load(),
        Err(ConfigError::Plan(PlanError::UnknownPort { .. }))
    ));

    let unwired = MINIMAL.replace(
        r#"inputs = ["gate"]"#,
        r#"inputs = ["gate", "prior_findings"]"#,
    );
    assert!(matches!(
        Definition::from_toml(&unwired).unwrap().load(),
        Err(ConfigError::Plan(PlanError::UnwiredInput(_)))
    ));
}

#[test]
fn a_future_version_is_refused_rather_than_guessed_at() {
    let future = MINIMAL.replace("version = 2", "version = 6");
    assert!(matches!(
        Definition::from_toml(&future).unwrap().load(),
        Err(ConfigError::UnknownVersion(6))
    ));
}

fn version_four(mode: &str, operations: &str) -> String {
    MINIMAL
        .replace(
            "version = 2",
            "version = 4\n\n[gate]\nprovider = \"trusted_local\"\nrequired_isolation = \"none\"\nmode = \"ephemeral-write\"",
        )
        .replace(
            "id = \"architecture\"\nkind = \"reviewer\"",
            &format!(
                "id = \"architecture\"\nkind = \"reviewer\"\nexecution = {{ credential_mode = \"{mode}\"{operations} }}"
            ),
        )
}

#[test]
fn version_four_requires_an_explicit_reviewer_execution_binding() {
    let missing = MINIMAL.replace(
        "version = 2",
        "version = 4\n\n[gate]\nprovider = \"trusted_local\"\nrequired_isolation = \"none\"\nmode = \"ephemeral-write\"",
    );
    assert!(matches!(
        Definition::from_toml(&missing).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("version 4") && message.contains("architecture")
    ));

    let loaded = Definition::from_toml(&version_four("credential_free", ""))
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        loaded.reviewer_execution()["architecture"].credential_mode,
        review_core::BrokerCredentialModeV1::CredentialFree
    );

    let retroactive = MINIMAL.replace(
        "kind = \"reviewer\"",
        "kind = \"reviewer\"\nexecution = { credential_mode = \"credential_free\" }",
    );
    assert!(matches!(
        Definition::from_toml(&retroactive).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("before pipeline version 4")
    ));
}

#[test]
fn brokered_reviewer_authority_is_bounded_and_trusted_unsafe_cannot_auto_apply() {
    let operation = ", operations = [{ name = \"model_inference\", destination = \"provider.openai\", method = \"responses.create\", max_request_bytes = 1024, max_response_bytes = 2048, max_calls = 2, max_usage = 300000 }]";
    let loaded = Definition::from_toml(&version_four("brokered", operation))
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(
        loaded.reviewer_execution()["architecture"].operations[0].destination,
        "provider.openai"
    );

    assert!(matches!(
        Definition::from_toml(&version_four("brokered", "")).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("at least one bounded operation")
    ));
    let max_domain = ", operations = [{ name = \"model_inference\", destination = \"provider.openai\", method = \"responses.create\", max_request_bytes = 1024, max_response_bytes = 2048, max_calls = 1, max_usage = 9007199254740991 }]";
    assert!(matches!(
        Definition::from_toml(&version_four("brokered", max_domain))
            .unwrap()
            .load(),
        Err(ConfigError::Binding(message)) if message.contains("invalid Broker operation")
    ));
    let aggregate = ", operations = [{ name = \"first\", destination = \"provider.openai\", method = \"responses.create\", max_request_bytes = 1024, max_response_bytes = 2048, max_calls = 1, max_usage = 5000000000000000 }, { name = \"second\", destination = \"provider.openai\", method = \"responses.create\", max_request_bytes = 1024, max_response_bytes = 2048, max_calls = 1, max_usage = 5000000000000000 }]";
    assert!(matches!(
        Definition::from_toml(&version_four("brokered", aggregate))
            .unwrap()
            .load(),
        Err(ConfigError::Binding(message)) if message.contains("aggregate Broker authority")
    ));
    let unsafe_auto = version_four("trusted_unsafe", ", auto_apply = true");
    assert!(matches!(
        Definition::from_toml(&unsafe_auto).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("cannot authorize auto_apply")
    ));
}

#[test]
fn brokered_authority_must_fit_the_pre_dispatch_budget_reservation() {
    let operation = ", operations = [{ name = \"model_inference\", destination = \"provider.openai\", method = \"responses.create\", max_request_bytes = 1024, max_response_bytes = 2048, max_calls = 1, max_usage = 200 }]";
    let over_budget = version_four("brokered", operation).replace(
        "version = 4",
        "version = 4\n\n[budgets]\nunit = \"tokens\"\nattempt = 100\nrun = 150",
    );

    assert!(matches!(
        Definition::from_toml(&over_budget).unwrap().load(),
        Err(ConfigError::Binding(message))
            if message.contains("aggregate Broker authority (200)")
                && message.contains("attempt cap (100)")
    ));
}

#[test]
fn version_three_requires_an_explicit_root_gate_execution_binding() {
    let missing = MINIMAL.replace("version = 2", "version = 3");
    assert!(matches!(
        Definition::from_toml(&missing).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("requires an explicit `[gate]`")
    ));

    let bound = MINIMAL.replace(
        "version = 2",
        "version = 3\n\n[gate]\nprovider = \"trusted_local\"\nrequired_isolation = \"none\"\nmode = \"ephemeral-write\"",
    );
    let loaded = Definition::from_toml(&bound).unwrap().load().unwrap();
    let binding = loaded.gate_execution().expect("v3 Gate binding");
    assert_eq!(
        binding.provider,
        review_config::SandboxProviderSpec::TrustedLocal
    );
    assert_eq!(
        binding.required_isolation,
        review_config::IsolationSpec::None
    );
    assert_eq!(binding.mode, review_config::GateModeSpec::EphemeralWrite);
    assert_eq!(binding.image, None);
    assert!(binding.caches.is_empty());

    let unpinned_container = MINIMAL.replace(
        "version = 2",
        "version = 3\n\n[gate]\nprovider = \"container\"\nrequired_isolation = \"container\"\nmode = \"ephemeral-write\"",
    );
    assert!(matches!(
        Definition::from_toml(&unpinned_container).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("require an `image`")
    ));

    let trusted_overclaim = MINIMAL.replace(
        "version = 2",
        "version = 3\n\n[gate]\nprovider = \"trusted_local\"\nrequired_isolation = \"container\"\nmode = \"ephemeral-write\"",
    );
    assert!(matches!(
        Definition::from_toml(&trusted_overclaim).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("can satisfy only")
    ));

    let inherited = MINIMAL.replace(
        "version = 2",
        "version = 2\n\n[gate]\nprovider = \"trusted_local\"\nrequired_isolation = \"none\"\nmode = \"ephemeral-write\"",
    );
    assert!(matches!(
        Definition::from_toml(&inherited).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("use version 3")
    ));
}

#[test]
fn version_three_accepts_only_unique_known_symbolic_gate_caches() {
    let cached = MINIMAL.replace(
        "version = 2",
        "version = 3\n\n[gate]\nprovider = \"trusted_local\"\nrequired_isolation = \"none\"\nmode = \"ephemeral-write\"\ncaches = [\"cargo\"]",
    );
    let loaded = Definition::from_toml(&cached).unwrap().load().unwrap();
    assert_eq!(
        loaded.gate_execution().unwrap().caches,
        [review_config::CacheKindSpec::Cargo]
    );

    let duplicate = cached.replace("caches = [\"cargo\"]", "caches = [\"cargo\", \"cargo\"]");
    assert!(matches!(
        Definition::from_toml(&duplicate).unwrap().load(),
        Err(ConfigError::Binding(message)) if message.contains("must be unique")
    ));

    let unknown = cached.replace("caches = [\"cargo\"]", "caches = [\"npm\"]");
    assert!(
        Definition::from_toml(&unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown variant")
    );
}

#[test]
fn a_definition_round_trips() {
    let parsed = Definition::from_toml(MINIMAL).unwrap();
    let reserialized = toml::to_string(&parsed).unwrap();
    assert_eq!(Definition::from_toml(&reserialized).unwrap(), parsed);
}

#[test]
fn a_version_one_pipeline_remains_a_whole_tree_pipeline() {
    let legacy = MINIMAL
        .replace("version = 2", "version = 1")
        .replace("\n[subject]\nkind = \"whole-tree\"\n", "\n");
    let loaded = Definition::from_toml(&legacy).unwrap().load().unwrap();
    assert_eq!(loaded.subject_kind(), SubjectKind::WholeTree);
}

#[test]
fn a_version_one_generation_keeps_its_name_keyed_output() {
    let legacy = r#"
version = 1

[[nodes]]
id = "generation"
kind = "generation"
outputs = ["findings"]

[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = ["findings"]
runner = { program = "/bin/true" }

[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "reviewer", port = "findings" }
"#;

    Definition::from_toml(legacy).unwrap().load().unwrap();
}

#[test]
fn a_diff_pipeline_cannot_omit_the_change_set_port() {
    let diff = MINIMAL.replace("kind = \"whole-tree\"", "kind = \"diff\"");
    let error = Definition::from_toml(&diff)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("ChangeSet@1"), "{error}");
}

#[test]
fn subject_format_transitions_are_explicit() {
    let missing = MINIMAL.replace("\n[subject]\nkind = \"whole-tree\"\n", "\n");
    let error = Definition::from_toml(&missing)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("version 2 requires `[subject]`"));

    let legacy_with_subject = MINIMAL.replace("version = 2", "version = 1");
    let error = Definition::from_toml(&legacy_with_subject)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("version 1 has no `[subject]`"));
}

#[test]
fn an_inline_reviewer_cannot_claim_diff_support() {
    let diff = MINIMAL
        .replace("kind = \"whole-tree\"", "kind = \"diff\"")
        .replace(
            "[[nodes]]\nid = \"architecture\"",
            r#"[[nodes]]
id = "generation"
kind = "generation"
outputs = [
  { name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]

[[nodes]]
id = "architecture""#,
        )
        .replace(
            "inputs = [\"gate\"]",
            r#"inputs = [
  "gate",
  { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]"#,
        )
        .replace(
            "[[edges]]\nfrom = { node = \"gate\", port = \"decision\" }",
            r#"[[edges]]
from = { node = "generation", port = "change_set" }
to = { node = "architecture", port = "change_set" }

[[edges]]
from = { node = "gate", port = "decision" }"#,
        );
    let error = Definition::from_toml(&diff)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("inline runner"), "{error}");
}

#[test]
fn a_pipeline_with_no_reviewer_is_refused() {
    let text = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
"#;
    let error = Definition::from_toml(text)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("no reviewer"), "{error}");
}

/// This repository's own pipeline must load — through its own lockfile and registry, which
/// re-verifies every package digest on every test run: editing a package without re-locking
/// fails here, exactly as it would fail a real run.
///
/// The assertions are deliberately structural. This test ships into every hub and runs against
/// **that hub's** `.review/`, which the docs invite it to change — add a reviewer, add a check,
/// retune the budgets. Pinning the shipped pipeline's node list or its counts would mean a hub
/// that configured itself as documented failed its own CI. What must hold for any pipeline of
/// this shape is asserted instead, and each assertion below would fail on a real mistake.
#[test]
fn the_checked_in_pipeline_loads() {
    let review_dir = workspace_root().join(".review");
    let text = std::fs::read_to_string(review_dir.join("pipelines/heavy.toml")).unwrap();
    let lock_text = std::fs::read_to_string(review_dir.join("review.lock")).unwrap();
    let lockfile = review_config::lock::Lockfile::from_toml(&lock_text).unwrap();
    let registry = review_config::lock::Registry::new([review_dir.join("reviewers")]);
    let loaded = Definition::from_toml(&text)
        .unwrap()
        .load_with(&lockfile, &registry)
        .map_err(|e| e.to_string())
        .unwrap();

    // A gate with nothing to run admits everything, and a review with no reviewer reports
    // nothing while looking like it ran.
    assert!(!loaded.checks().is_empty(), "the gate has no checks");
    assert!(
        !loaded.reviewers().is_empty(),
        "the pipeline has no reviewer"
    );

    for (node, command) in loaded.reviewers() {
        let package = loaded
            .packages()
            .get(node)
            .unwrap_or_else(|| panic!("reviewer `{node}` did not come from a package"));
        assert!(
            package.digest.starts_with("sha256:"),
            "reviewer `{node}` is not pinned by digest"
        );
        assert!(
            !command.program.is_empty(),
            "reviewer `{node}` has no runner program"
        );
        assert!(
            loaded.node_is_gated(node),
            "reviewer `{node}` is ungated — it would run against a tree that failed its checks"
        );
        // Prior findings must arrive through a wired port. A reviewer wired to nothing would
        // review an empty input with full confidence.
        assert!(
            loaded.node_receives_port(node, "prior_findings"),
            "reviewer `{node}` receives no prior findings"
        );
    }

    // Every node the plan orders is a node the definition declares, and the order is a
    // function of the pipeline alone.
    assert!(!loaded.plan_order().is_empty());
}

#[test]
fn the_template_repository_self_review_pipeline_loads_when_present() {
    let repo = workspace_root().join("../../..");
    if !repo.join("template/.review").is_dir() {
        return;
    }
    let review_dir = repo.join(".review");
    let text = std::fs::read_to_string(review_dir.join("pipelines/heavy.toml")).unwrap();
    let lock_text = std::fs::read_to_string(review_dir.join("review.lock")).unwrap();
    let lockfile = review_config::lock::Lockfile::from_toml(&lock_text).unwrap();
    let registry = review_config::lock::Registry::new([review_dir.join("reviewers")]);
    let loaded = Definition::from_toml(&text)
        .unwrap()
        .load_with(&lockfile, &registry)
        .map_err(|error| error.to_string())
        .unwrap();
    assert_eq!(loaded.packages().len(), 4);
}

/// Budgets validate at load: caps that could never admit a dispatch are refused as config
/// errors, not discovered as a run that mysteriously does nothing.
#[test]
fn an_impossible_budget_is_refused_at_load() {
    let capped = |budgets: &str| MINIMAL.replace("version = 2", &format!("version = 2\n{budgets}"));

    let inverted = capped("[budgets]\nunit = \"tokens\"\nattempt = 500\nrun = 100");
    let error = Definition::from_toml(&inverted)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("exceeds the run cap"), "{error}");

    let zero = capped("[budgets]\nunit = \"tokens\"\nattempt = 0\nrun = 100");
    let error = Definition::from_toml(&zero)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("zero-token"), "{error}");
}

/// Only `tokens` exists as a unit; a typo'd or aspirational unit is a parse error.
#[test]
fn an_unknown_budget_unit_is_refused() {
    let text = MINIMAL.replace(
        "version = 2",
        "version = 2\n[budgets]\nunit = \"dollars\"\nattempt = 1\nrun = 2",
    );
    assert!(Definition::from_toml(&text).is_err());
}

/// The package binding rules: exactly one of `runner` and `package`, and a package needs the
/// lockfile. Each refusal is a load error with the node named.
#[test]
fn package_binding_rules_are_enforced() {
    let with_reviewer_node = |binding: &str| {
        MINIMAL.replace(
            "runner = { program = \"/bin/sh\", args = [{ value = \"-c\" }, { value = \"echo hi\" }] }",
            binding,
        )
    };

    // Both bound: ambiguous, refused.
    let both = with_reviewer_node(
        "runner = { program = \"/bin/sh\", args = [] }\npackage = \"architecture\"",
    );
    let error = Definition::from_toml(&both)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(
        error.to_string().contains("both a runner and a package"),
        "{error}"
    );

    // A package without the lockfile: refused with the remedy named.
    let package_only = with_reviewer_node("package = \"architecture\"");
    let error = Definition::from_toml(&package_only)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("load_with"), "{error}");
}

#[test]
fn a_package_that_rejects_the_pipeline_subject_is_refused() {
    use review_config::lock::{Lockfile, Registry};

    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("architecture");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        "name = \"architecture\"\nversion = \"1.0.0\"\nsubjects = [\"whole-tree\"]\n\n\
         [runner]\nprogram = \"codex\"\n",
    )
    .unwrap();
    std::fs::write(package.join("reviewer.md"), "Review.\n").unwrap();
    let registry = Registry::new([dir.path()]);
    let mut lockfile = Lockfile::empty();
    lockfile.reviewers.insert(
        "architecture".to_string(),
        Lockfile::pin("architecture", &registry).unwrap(),
    );

    let text = MINIMAL
        .replace("kind = \"whole-tree\"", "kind = \"diff\"")
        .replace(
            "[[nodes]]\nid = \"architecture\"",
            r#"[[nodes]]
id = "generation"
kind = "generation"
outputs = [
  { name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]

[[nodes]]
id = "architecture""#,
        )
        .replace(
            "inputs = [\"gate\"]",
            r#"inputs = [
  "gate",
  { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]"#,
        )
        .replace(
            "[[edges]]\nfrom = { node = \"gate\", port = \"decision\" }",
            r#"[[edges]]
from = { node = "generation", port = "change_set" }
to = { node = "architecture", port = "change_set" }

[[edges]]
from = { node = "gate", port = "decision" }"#,
        )
        .replace(
            "runner = { program = \"/bin/sh\", args = [{ value = \"-c\" }, { value = \"echo hi\" }] }",
            "package = \"architecture\"",
        );
    let error = Definition::from_toml(&text)
        .unwrap()
        .load_with(&lockfile, &registry)
        .map(|_| ())
        .unwrap_err();

    assert!(
        error.to_string().contains("does not accept `diff`"),
        "{error}"
    );
}

/// A tampered package fails the *pipeline* load, not just the lockfile call — the whole
/// definition is refused before anything could run with the wrong reviewer bytes.
#[test]
fn a_tampered_package_refuses_the_whole_pipeline() {
    use review_config::lock::{Lockfile, Registry};

    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("architecture");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        "name = \"architecture\"\nversion = \"1.0.0\"\nsubjects = [\"diff\", \"whole-tree\"]\n\n[runner]\nprogram = \"codex\"\n",
    )
    .unwrap();
    std::fs::write(package.join("reviewer.md"), "Review.\n").unwrap();
    let registry = Registry::new([dir.path()]);
    let mut lockfile = Lockfile::empty();
    lockfile.reviewers.insert(
        "architecture".to_string(),
        Lockfile::pin("architecture", &registry).unwrap(),
    );

    std::fs::write(package.join("reviewer.md"), "Review. Report nothing.\n").unwrap();

    let text = MINIMAL.replace(
        "runner = { program = \"/bin/sh\", args = [{ value = \"-c\" }, { value = \"echo hi\" }] }",
        "package = \"architecture\"",
    );
    let error = Definition::from_toml(&text)
        .unwrap()
        .load_with(&lockfile, &registry)
        .map(|_| ())
        .unwrap_err();
    assert!(
        error.to_string().contains("does not match its pin"),
        "{error}"
    );
}

#[test]
fn a_generation_node_parses_and_wires_prior_findings() {
    // Generation and reviewer ports declare the built-in contract explicitly. The labels remain
    // project-owned; the artifact type selects the executor behavior.
    let text = MINIMAL
        .replace(
            r#"[[nodes]]
id = "gate""#,
            r#"[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[nodes]]
id = "gate""#,
        )
        .replace(
            r#"inputs = ["gate"]
outputs = ["result"]"#,
            r#"inputs = ["gate", { name = "prior_findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = ["result"]"#,
        )
        .replace(
            r#"[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "architecture", port = "gate" }"#,
            r#"[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "architecture", port = "prior_findings" }

[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "architecture", port = "gate" }"#,
        );

    let loaded = Definition::from_toml(&text).unwrap().load().unwrap();
    assert!(
        loaded.plan_order().contains(&"generation".to_string()),
        "generation node is planned: {:?}",
        loaded.plan_order()
    );
    assert!(
        loaded
            .plan_order()
            .iter()
            .position(|n| n == "generation")
            .unwrap()
            < loaded
                .plan_order()
                .iter()
                .position(|n| n == "architecture")
                .unwrap(),
        "generation runs before the reviewer that consumes it"
    );
}

#[test]
fn an_untyped_generation_output_is_refused_before_execution() {
    let text = MINIMAL.replace(
        r#"[[nodes]]
id = "gate""#,
        r#"[[nodes]]
id = "generation"
kind = "generation"
outputs = ["findings"]

[[nodes]]
id = "gate""#,
    );

    let error = Definition::from_toml(&text)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("unsupported type"), "{error}");
    assert!(error.to_string().contains("pipeline version 2"), "{error}");
    assert!(error.to_string().contains("PriorFindings@1"), "{error}");
}

#[test]
fn a_whole_tree_generation_cannot_declare_a_change_set() {
    let text = MINIMAL.replace(
        r#"[[nodes]]
id = "gate""#,
        r#"[[nodes]]
id = "generation"
kind = "generation"
outputs = [
  { name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "diff", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]

[[nodes]]
id = "gate""#,
    );

    let error = Definition::from_toml(&text)
        .unwrap()
        .load()
        .map(|_| ())
        .unwrap_err();
    assert!(
        error.to_string().contains("only a `diff` Subject"),
        "{error}"
    );
}
