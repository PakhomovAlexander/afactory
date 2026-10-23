//! The Store writer marker (`af/store-writer@1`): the record each release keeps beside a
//! Store's event log, which names that release when another one cannot replay the Store.

use super::{assert_invalid, assert_valid};
use review_core::store_writer::{STORE_WRITER_V1, StoreWriterV1};
use serde_json::json;

#[test]
fn the_store_writer_marker_and_its_schema_agree() {
    let marker = StoreWriterV1::new("0.12.0-rc.5", 1_792_728_726_048);
    marker.validate().unwrap();
    let value = serde_json::to_value(&marker).unwrap();
    assert_eq!(value["schema"], STORE_WRITER_V1);
    assert_valid("store-writer-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<StoreWriterV1>(value.clone()).unwrap(),
        marker
    );

    let mut extra = value.clone();
    extra["af_commit"] = json!("abc");
    assert_invalid("store-writer-v1.json", &extra, "undeclared field");
    assert!(serde_json::from_value::<StoreWriterV1>(extra).is_err());

    for (field, replacement, why) in [
        ("schema", json!("af/store-writer@2"), "another version"),
        ("af_version", json!("v0.12"), "not a release spelling"),
        ("af_version", json!(""), "empty release"),
        ("af_version", json!("1".repeat(65)), "release too long"),
        ("first_unix_ms", json!(-1), "negative time"),
        ("first_unix_ms", json!("1792728726048"), "time as text"),
    ] {
        let mut changed = value.clone();
        changed[field] = replacement;
        assert_invalid("store-writer-v1.json", &changed, why);
        let parsed = serde_json::from_value::<StoreWriterV1>(changed);
        assert!(
            parsed.is_err() || parsed.unwrap().validate().is_err(),
            "{why}"
        );
    }
}
