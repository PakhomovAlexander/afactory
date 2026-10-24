//! The warm check layer's records (ADR-0207): `af.warm-sources/1`, `af.warm-holder/1`,
//! `af.warm-eviction/1` and `af/TaskCheckLayer@1`. A fully populated value of each is valid
//! against its schema and the Rust validator and round-trips; every shape either side refuses
//! the other refuses too.

use super::*;
use review_core::task::check_layer::*;
use std::collections::BTreeMap;

fn digest(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

fn sources() -> WarmSourcesV1 {
    WarmSourcesV1::new(BTreeMap::from([
        (
            String::new(),
            WarmSourceGroupV1 {
                digest: digest('a'),
                stamp_unix_ns: "1761210000123456789".into(),
            },
        ),
        (
            "crates/review-core".into(),
            WarmSourceGroupV1 {
                digest: digest('b'),
                stamp_unix_ns: "1761210000000000001".into(),
            },
        ),
    ]))
}

fn layer() -> TaskCheckLayerV1 {
    let key = format!("{}/{}", "a".repeat(64), "b".repeat(64));
    TaskCheckLayerV1 {
        receipt_id: digest('c'),
        checks: BTreeMap::from([
            (
                "kernel".to_string(),
                TaskCheckLayerRunV1 {
                    key: Some(key.clone()),
                    lock: TaskLayerLockV1::Waited,
                    waited_ms: 61_000,
                    holder: Some(TaskLayerHolderV1 {
                        pid: 4242,
                        host: "claw-3".into(),
                        since_unix_ms: 1_761_210_000_000,
                    }),
                    start: TaskLayerStartV1::Warm,
                    sources: TaskLayerSourcesV1 {
                        stale: 2,
                        fresh: 15,
                    },
                    phases: TaskLayerPhasesV1 {
                        build_wall_ms: Some(2_400_000),
                        check_wall_ms: 3_600_000,
                        build_ms: Some(812_000),
                        tests_ms: Some(1_530_000),
                        elapsed_ms: 2_342_000,
                        deadline: Some(TaskLayerPhaseV1::Tests),
                    },
                    evicted: Some(TaskLayerEvictionV1 {
                        phase: TaskLayerPhaseV1::Build,
                        by: TaskLayerEvictorV1::StorageSweep,
                        at_unix_ms: 1_761_210_100_000,
                    }),
                },
            ),
            (
                "markdownlint".to_string(),
                TaskCheckLayerRunV1 {
                    key: None,
                    lock: TaskLayerLockV1::Unresolved,
                    waited_ms: 0,
                    holder: None,
                    start: TaskLayerStartV1::Cold,
                    sources: TaskLayerSourcesV1 { stale: 0, fresh: 0 },
                    phases: TaskLayerPhasesV1 {
                        build_wall_ms: Some(2_400_000),
                        check_wall_ms: 3_600_000,
                        build_ms: None,
                        tests_ms: None,
                        elapsed_ms: 900,
                        deadline: None,
                    },
                    evicted: None,
                },
            ),
        ]),
    }
}

fn layer_accepted(value: &Value) -> bool {
    serde_json::from_value::<TaskCheckLayerV1>(value.clone())
        .is_ok_and(|layer| layer.validate().is_ok())
}

#[test]
fn warm_layer_records_are_valid_and_round_trip() {
    let record = sources();
    record.validate().unwrap();
    let value = serde_json::to_value(&record).unwrap();
    assert_valid("warm-sources-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<WarmSourcesV1>(value).unwrap(),
        record
    );

    let holder = WarmHolderV1 {
        schema: WARM_HOLDER_V1.into(),
        pid: 4242,
        host: "claw-3".into(),
        since_unix_ms: 1_761_210_000_000,
        command: "af task run".into(),
    };
    holder.validate().unwrap();
    let value = serde_json::to_value(&holder).unwrap();
    assert_valid("warm-holder-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<WarmHolderV1>(value).unwrap(),
        holder
    );

    let eviction = WarmEvictionV1 {
        schema: WARM_EVICTION_V1.into(),
        by: WarmEvictorV1::StorageSweep,
        at_unix_ms: 1_761_210_100_000,
        bytes: 26_521_100_000,
    };
    eviction.validate().unwrap();
    let value = serde_json::to_value(&eviction).unwrap();
    assert_valid("warm-eviction-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<WarmEvictionV1>(value).unwrap(),
        eviction
    );

    let layer = layer();
    layer.validate().unwrap();
    let value = serde_json::to_value(&layer).unwrap();
    assert_valid("task-check-layer-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskCheckLayerV1>(value).unwrap(),
        layer
    );
}

#[test]
fn warm_record_shapes_either_side_refuses_the_other_refuses() {
    let base = serde_json::to_value(sources()).unwrap();
    for (why, value) in [
        ("another schema", {
            let mut v = base.clone();
            v["schema"] = json!("af.warm-sources/2");
            v
        }),
        ("an absolute group", {
            let mut v = base.clone();
            v["groups"]["/abs"] = v["groups"][""].clone();
            v
        }),
        ("a parent component", {
            let mut v = base.clone();
            v["groups"]["a/../b"] = v["groups"][""].clone();
            v
        }),
        ("a stamp with a leading zero", {
            let mut v = base.clone();
            v["groups"][""]["stamp_unix_ns"] = json!("0123");
            v
        }),
        ("a numeric stamp", {
            let mut v = base.clone();
            v["groups"][""]["stamp_unix_ns"] = json!(1);
            v
        }),
        ("an unknown field", {
            let mut v = base.clone();
            v["groups"][""]["mtime"] = json!("1");
            v
        }),
    ] {
        assert_invalid("warm-sources-v1.json", &value, why);
        assert!(
            serde_json::from_value::<WarmSourcesV1>(value)
                .map_or(true, |record| record.validate().is_err()),
            "Rust accepted {why}"
        );
    }

    let holder = json!({"schema":"af.warm-holder/1","pid":0,"host":"h","since_unix_ms":1,"command":"af task run"});
    assert_invalid("warm-holder-v1.json", &holder, "pid 0");
    assert!(
        serde_json::from_value::<WarmHolderV1>(holder)
            .unwrap()
            .validate()
            .is_err()
    );
    let eviction = json!({"schema":"af.warm-eviction/1","by":"operator","at_unix_ms":1,"bytes":1});
    assert_invalid("warm-eviction-v1.json", &eviction, "an unknown evictor");
    assert!(serde_json::from_value::<WarmEvictionV1>(eviction).is_err());

    let base = serde_json::to_value(layer()).unwrap();
    let mutate = |edit: &dyn Fn(&mut Value)| {
        let mut value = base.clone();
        edit(&mut value);
        value
    };
    for (why, value) in [
        ("unknown field", mutate(&|v| v["verdict"] = json!("passed"))),
        ("no checks", mutate(&|v| v["checks"] = json!({}))),
        (
            "a waited layer without its key",
            mutate(&|v| {
                v["checks"]["kernel"].as_object_mut().unwrap().remove("key");
            }),
        ),
        (
            "an unresolved toolchain with a key",
            mutate(&|v| v["checks"]["markdownlint"]["key"] = v["checks"]["kernel"]["key"].clone()),
        ),
        (
            "a holder of a layer held at once",
            mutate(&|v| v["checks"]["kernel"]["lock"] = json!("held")),
        ),
        (
            "a short key",
            mutate(&|v| v["checks"]["kernel"]["key"] = json!("ab/cd")),
        ),
        (
            "tests without a build",
            mutate(&|v| {
                v["checks"]["kernel"]["phases"]
                    .as_object_mut()
                    .unwrap()
                    .remove("build_ms");
            }),
        ),
        (
            "a zero build deadline",
            mutate(&|v| v["checks"]["kernel"]["phases"]["build_wall_ms"] = json!(0)),
        ),
        (
            "a phase deadline without a declared build deadline",
            mutate(&|v| {
                v["checks"]["kernel"]["phases"]
                    .as_object_mut()
                    .unwrap()
                    .remove("build_wall_ms");
            }),
        ),
        (
            "a deadline after the check",
            mutate(&|v| v["checks"]["kernel"]["phases"]["deadline"] = json!("after")),
        ),
        (
            "a kernel eviction during the build",
            mutate(&|v| v["checks"]["kernel"]["evicted"]["by"] = json!("bound")),
        ),
        (
            "a sweep eviction after the check",
            mutate(&|v| v["checks"]["kernel"]["evicted"]["phase"] = json!("after")),
        ),
        (
            "an unknown lock state",
            mutate(&|v| v["checks"]["kernel"]["lock"] = json!("stolen")),
        ),
    ] {
        assert_invalid("task-check-layer-v1.json", &value, why);
        assert!(!layer_accepted(&value), "Rust accepted {why}");
    }
}

#[test]
fn the_seal_ignored_record_and_its_schema_agree() {
    use review_core::task::seal::*;
    let record = SealIgnoredV1::new(
        digest('d'),
        vec!["pkg/__pycache__/m.pyc".into(), "a.pyc".into()],
    );
    record.validate().unwrap();
    let value = serde_json::to_value(&record).unwrap();
    assert_valid("seal-ignored-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<SealIgnoredV1>(value.clone()).unwrap(),
        record
    );
    for (why, edit) in [
        ("no paths", json!({"ignored": []})),
        ("an absolute path", json!({"ignored": ["/etc/passwd"]})),
        ("a zero count", json!({"ignored_count": 0})),
        ("an unknown field", json!({"refused": []})),
    ] {
        let mut changed = value.clone();
        for (key, field) in edit.as_object().unwrap() {
            changed[key] = field.clone();
        }
        assert_invalid("seal-ignored-v1.json", &changed, why);
        assert!(
            serde_json::from_value::<SealIgnoredV1>(changed)
                .map_or(true, |record| record.validate().is_err()),
            "Rust accepted {why}"
        );
    }
}
