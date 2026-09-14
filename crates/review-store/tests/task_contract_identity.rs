use review_store::canonical::{canonicalize, content_id};
use serde_json::Value;

#[path = "../../review-core/tests/support/task_fixtures.rs"]
mod corpus;

#[test]
fn task_contract_fixtures_retain_canonical_content_identity() {
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("fixtures/task-contracts/v1");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("content-ids.json")).unwrap()).unwrap();
    for (file, expected) in manifest.as_object().unwrap() {
        let value: Value =
            serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
        review_core::json::admit(&value).unwrap();
        assert_eq!(
            content_id(&value).unwrap(),
            expected.as_str().unwrap(),
            "{file}"
        );
        let canonical = canonicalize(&value).unwrap();
        let restored: Value = serde_json::from_slice(&canonical).unwrap();
        assert_eq!(
            content_id(&restored).unwrap(),
            expected.as_str().unwrap(),
            "{file}"
        );
        assert_eq!(canonicalize(&restored).unwrap(), canonical, "{file}");
        let typed = corpus::typed_round_trip(file.strip_suffix(".json").unwrap(), value).unwrap();
        assert_eq!(
            content_id(&typed).unwrap(),
            expected.as_str().unwrap(),
            "typed {file}"
        );
    }
    let cases: Value =
        serde_json::from_slice(&std::fs::read(root.join("negative.json")).unwrap()).unwrap();
    for case in cases.as_array().unwrap() {
        let name = case["contract"].as_str().unwrap();
        let base: Value =
            serde_json::from_slice(&std::fs::read(root.join(format!("{name}.json"))).unwrap())
                .unwrap();
        let invalid = corpus::expand(&base, case);
        // Fingerprint the intended invalid payload, including unsafe-number negative cases.
        // This is fixture identity evidence, not numeric-domain or Store admission.
        assert_eq!(
            content_id(&invalid).unwrap(),
            case["expected_content_id"].as_str().unwrap(),
            "{}",
            case["why"]
        );
        assert!(
            corpus::typed_round_trip(name, invalid).is_err(),
            "{}",
            case["why"]
        );
    }
}

#[test]
fn every_task_set_round_trips_canonically_and_rejects_permuted_wire_order() {
    use serde_json::json;
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("fixtures/task-contracts/v1");
    let read = |name: &str| -> Value {
        serde_json::from_slice(&std::fs::read(root.join(format!("{name}.json"))).unwrap()).unwrap()
    };
    let mut task = read("task-revision");
    task["authority"]["allowed_effects"] = json!(["read-source", "write-source"]);
    task["authority"]["data_destinations"] = json!(["alpha", "beta"]);
    let mut plan = read("execution-plan");
    plan["authority"] = task["authority"].clone();
    plan["acceptance"]["checked"] = json!(["verify.output", "write.output"]);
    let mut pipeline = read("pipeline-definition");
    pipeline["contract"]["outputs"]["document"]["covers"] = json!(["checked", "documented"]);
    pipeline["coverage"]["documented"] = pipeline["coverage"]["checked"].clone();
    pipeline["accepts"]["kinds"] = json!(["document", "review"]);
    let mut slot = pipeline["slots"]["author"].clone();
    slot["min_attempts"] = json!(0);
    pipeline["slots"]["checker"] = slot.clone();
    pipeline["slots"]["verifier"] = slot;
    pipeline["slots"]["author"]["independent_from"] = json!(["checker", "verifier"]);
    pipeline["nodes"].as_array_mut().unwrap().push(
        json!({"id":"checks", "operator":{"op":"check", "checks":["lint", "test"]}, "inputs":{}}),
    );
    let mut result = read("task-result");
    result["acceptance"] = json!("unsatisfied");
    result["missing_obligations"] = json!(["checked", "documented"]);
    result["evidence"]
        .as_array_mut()
        .unwrap()
        .push(json!(format!("sha256:{}", "f".repeat(64))));

    fn set_paths(value: &Value, path: &str, contract: &str, out: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    let next = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                    if child.is_array()
                        && (matches!(
                            key.as_str(),
                            "allowed_effects"
                                | "data_destinations"
                                | "evidence"
                                | "missing_obligations"
                                | "covers"
                                | "independent_from"
                                | "kinds"
                                | "checks"
                        ) || (contract == "execution-plan" && path == "/acceptance"))
                    {
                        out.push(next.clone());
                    }
                    set_paths(child, &next, contract, out);
                }
            }
            Value::Array(array) => {
                for (i, child) in array.iter().enumerate() {
                    set_paths(child, &format!("{path}/{i}"), contract, out);
                }
            }
            _ => {}
        }
    }
    let mut permutations = 0;
    for (contract, value) in [
        ("task-revision", task),
        ("execution-plan", plan),
        ("pipeline-definition", pipeline),
        ("task-result", result),
    ] {
        let typed = corpus::typed_round_trip(contract, value.clone()).unwrap();
        assert_eq!(
            content_id(&typed).unwrap(),
            content_id(&value).unwrap(),
            "{contract}"
        );
        let mut paths = Vec::new();
        set_paths(&value, "", contract, &mut paths);
        for path in paths {
            if value.pointer(&path).unwrap().as_array().unwrap().len() < 2 {
                continue;
            }
            let mut unsorted = value.clone();
            unsorted
                .pointer_mut(&path)
                .unwrap()
                .as_array_mut()
                .unwrap()
                .reverse();
            assert!(
                corpus::typed_round_trip(contract, unsorted).is_err(),
                "{contract}{path} admitted an identity-changing permutation"
            );
            permutations += 1;
        }
    }
    assert_eq!(
        permutations, 11,
        "every set family must have a nontrivial permutation case"
    );
}
