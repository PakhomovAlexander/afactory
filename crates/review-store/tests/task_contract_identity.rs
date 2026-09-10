use review_store::canonical::{canonicalize, content_id};
use serde_json::Value;

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
    }
}
