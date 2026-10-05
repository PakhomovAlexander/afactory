//! `af/RemoteCheckEvidence@1` (ADR-0140): every recorded fixture of the three states is valid
//! against the schema and the Rust validator, round-trips byte for byte, and derives the status
//! its name states; every shape either side refuses the other refuses too.

use super::*;
use review_core::task::remote_check::{
    RemoteCheckEvidenceV1, RemoteCheckReasonV1, RemoteCheckVerdictV1,
};

fn fixture(name: &str) -> Value {
    let path = workspace_root()
        .join("fixtures/remote-checks/evidence")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}")))
        .unwrap()
}

fn rust_accepts(value: &Value) -> bool {
    serde_json::from_value::<RemoteCheckEvidenceV1>(value.clone())
        .is_ok_and(|evidence| evidence.validate().is_ok())
}

#[test]
fn recorded_evidence_of_every_state_is_valid_and_derives_its_status() {
    for (name, verdict) in [
        (
            "refused-skipped-local-failed.json",
            RemoteCheckVerdictV1::NotRun(RemoteCheckReasonV1::RemoteSkippedLocalFailed),
        ),
        (
            "refused-push-refused.json",
            RemoteCheckVerdictV1::NotRun(RemoteCheckReasonV1::RemotePushRefused),
        ),
        (
            "published-check-missing.json",
            RemoteCheckVerdictV1::NotRun(RemoteCheckReasonV1::RemoteCheckMissing),
        ),
        (
            "published-deadline-expired.json",
            RemoteCheckVerdictV1::NotRun(RemoteCheckReasonV1::DeadlineExpired),
        ),
        ("observed-passed.json", RemoteCheckVerdictV1::Passed),
        (
            "observed-passed-trusted-ci.json",
            RemoteCheckVerdictV1::Passed,
        ),
        ("observed-failed.json", RemoteCheckVerdictV1::Failed),
        (
            "observed-inconclusive.json",
            RemoteCheckVerdictV1::NotRun(RemoteCheckReasonV1::RemoteCheckInconclusive),
        ),
    ] {
        let value = fixture(name);
        assert_valid("remote-check-evidence-v1.json", &value);
        let evidence: RemoteCheckEvidenceV1 = serde_json::from_value(value.clone()).unwrap();
        evidence
            .validate()
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(evidence.verdict(), verdict, "{name}");
        assert_eq!(serde_json::to_value(&evidence).unwrap(), value, "{name}");
    }
}

#[test]
fn evidence_shapes_either_side_refuses_the_other_refuses() {
    let observed = fixture("observed-passed.json");
    let refused = fixture("refused-skipped-local-failed.json");
    let published = fixture("published-check-missing.json");
    let mutate = |base: &Value, edit: &dyn Fn(&mut Value)| {
        let mut value = base.clone();
        edit(&mut value);
        value
    };
    let trusted = fixture("observed-passed-trusted-ci.json");
    for (why, value) in [
        (
            "unknown field",
            mutate(&observed, &|v| v["log"] = json!("secret")),
        ),
        (
            "a trusted CI exception under another spelling of the tag",
            mutate(&trusted, &|v| v["trusted_ci"]["tag"] = json!("CI")),
        ),
        (
            "a trusted CI exception under a tag that merely contains ci",
            mutate(&trusted, &|v| v["trusted_ci"]["tag"] = json!("cicd")),
        ),
        (
            "a trusted CI exception without its plan",
            mutate(&trusted, &|v| {
                v["trusted_ci"].as_object_mut().unwrap().remove("plan_id");
            }),
        ),
        (
            "a trusted CI exception with an unknown field",
            mutate(&trusted, &|v| v["trusted_ci"]["granted"] = json!(true)),
        ),
        (
            "a trusted CI exception naming a mutable authority",
            mutate(&trusted, &|v| {
                v["trusted_ci"]["authority_id"] = json!("HEAD")
            }),
        ),
        (
            "a null trusted CI exception",
            mutate(&observed, &|v| v["trusted_ci"] = Value::Null),
        ),
        (
            "a trusted CI exception beside the .github refusal",
            mutate(&refused, &|v| {
                v["reason"] = json!("remote_candidate_changes_ci");
                v["trusted_ci"] = trusted["trusted_ci"].clone();
            }),
        ),
        (
            "unknown state",
            mutate(&observed, &|v| v["state"] = json!("pending")),
        ),
        (
            "unknown executor",
            mutate(&observed, &|v| v["executor"] = json!("gitlab-mr")),
        ),
        (
            "observed without merge",
            mutate(&observed, &|v| {
                v.as_object_mut().unwrap().remove("merge_commit");
            }),
        ),
        (
            "observed without run",
            mutate(&observed, &|v| {
                v.as_object_mut().unwrap().remove("run");
            }),
        ),
        (
            "observed with a diagnostic",
            mutate(&observed, &|v| v["diagnostic"] = json!("x")),
        ),
        (
            "observed with a foreign reason",
            mutate(&observed, &|v| {
                v["reason"] = json!("remote_check_missing");
            }),
        ),
        (
            "refused with a commit",
            mutate(&refused, &|v| v["base_commit"] = json!("a".repeat(40))),
        ),
        (
            "refused without reason",
            mutate(&refused, &|v| {
                v.as_object_mut().unwrap().remove("reason");
            }),
        ),
        (
            "refused for a merge mismatch",
            mutate(&refused, &|v| {
                v["reason"] = json!("remote_merge_mismatch");
            }),
        ),
        (
            "published without tree",
            mutate(&published, &|v| {
                v.as_object_mut().unwrap().remove("tree");
            }),
        ),
        (
            "published with jobs",
            mutate(&published, &|v| v["jobs"] = observed["jobs"].clone()),
        ),
        (
            "published for a local failure",
            mutate(&published, &|v| {
                v["reason"] = json!("remote_skipped_local_failed");
            }),
        ),
        (
            "a long diagnostic",
            mutate(&published, &|v| v["diagnostic"] = json!("x".repeat(2049))),
        ),
        (
            "an upper-case commit",
            mutate(&published, &|v| v["head_commit"] = json!("B".repeat(40))),
        ),
        (
            "a plain-http url",
            mutate(&observed, &|v| {
                v["pull_request"]["url"] = json!("http://github.com/o/r/pull/1");
            }),
        ),
        (
            "a successful job with steps",
            mutate(&observed, &|v| {
                v["jobs"][0]["steps"] = json!([{"name": "x", "conclusion": "failure"}]);
            }),
        ),
        (
            "a control character in a job name",
            mutate(&observed, &|v| {
                v["jobs"][0]["name"] = json!("validation /\u{7} lint");
                v["required"][0] = json!("validation /\u{7} lint");
            }),
        ),
        (
            "a workflow outside .github/workflows",
            mutate(&observed, &|v| {
                v["workflow"] = json!("ci.yml");
                v["run"]["workflow"] = json!("ci.yml");
            }),
        ),
        (
            "duplicate required names",
            mutate(&published, &|v| {
                v["required"] = json!(["a", "a"]);
            }),
        ),
    ] {
        assert!(!rust_accepts(&value), "Rust accepted {why}");
        assert_invalid("remote-check-evidence-v1.json", &value, why);
    }
    // Cross-field rules only the Rust validator states: the jobs are exactly the required
    // names in order, the run names the declared workflow, and the stated reason is the one
    // the job conclusions derive.
    for value in [
        mutate(&observed, &|v| {
            let jobs = v["jobs"].as_array_mut().unwrap();
            jobs.reverse();
        }),
        mutate(&observed, &|v| {
            v["run"]["workflow"] = json!(".github/workflows/other.yml")
        }),
        mutate(&observed, &|v| {
            v["reason"] = json!("remote_check_inconclusive")
        }),
        mutate(&fixture("observed-inconclusive.json"), &|v| {
            v.as_object_mut().unwrap().remove("reason");
        }),
    ] {
        assert!(!rust_accepts(&value), "Rust accepted {value}");
    }
}

/// `af task show --json` carries each remote check's evidence document under
/// `remote_checks` (ADR-0140): an entry names its node, check, status and evidence artifact.
#[test]
fn inspection_remote_checks_carry_the_evidence_document() {
    let inspection = schema("task-inspection-v11.json");
    let mut property = inspection["properties"]["remote_checks"].clone();
    property["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    let registry = jsonschema::Registry::new()
        .add(
            "urn:af:schema:remote-check-evidence:1",
            jsonschema::Resource::from_contents(schema("remote-check-evidence-v1.json")),
        )
        .unwrap()
        .prepare()
        .unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry)
        .build(&property)
        .unwrap();
    let entry = |record: Value| {
        json!({"node": "root.nodes.check", "check": "kernel", "status": "passed",
            "artifact_id": format!("sha256:{}", "e".repeat(64)),
            "artifact_type": "af/RemoteCheckEvidence@1", "record": record})
    };
    assert!(validator.is_valid(&json!([entry(fixture("observed-passed.json"))])));
    assert!(!validator.is_valid(&json!([])), "absent rather than empty");
    let mut forged = fixture("observed-passed.json");
    forged["log"] = json!("secret");
    assert!(!validator.is_valid(&json!([entry(forged)])));
    let mut typed = entry(fixture("observed-passed.json"));
    typed["artifact_type"] = json!("af/TaskRuntimeEvidence@1");
    assert!(!validator.is_valid(&json!([typed])));
}
