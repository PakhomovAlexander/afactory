use super::{assert_invalid, assert_valid};
use review_core::task::execution::{
    TaskExecutionRecordV1, TaskExecutionRecordV4, TaskExecutionRecordV5,
};
use review_core::task::owned_children::{TaskOwnedChildSetV1, TaskOwnedChildV1};
use serde_json::json;

fn id(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

#[test]
fn experimental_execution_records_have_a_distinct_generation() {
    for record in [
        TaskExecutionRecordV5::ExperimentPrepared {
            prepared_id: id('1'),
        },
        TaskExecutionRecordV5::ExperimentPlanDecided {
            prepared_id: id('1'),
            decision_id: id('2'),
        },
        TaskExecutionRecordV5::ExperimentChildrenRegistered {
            prepared_id: id('1'),
            decision_id: id('2'),
            child_plan_id: id('3'),
        },
    ] {
        record.validate().unwrap();
        let value = serde_json::to_value(&record).unwrap();
        assert_valid("task-execution-record-v5.json", &value);
        assert!(serde_json::from_value::<TaskExecutionRecordV1>(value).is_err());
    }
}

#[test]
fn owned_inspection_keeps_frozen_execution_generations_and_exact_registry_types() {
    let mut value = json!({
        "schema":"af/task-inspection@5", "task_id":"review-task", "revision_id":id('1'),
        "phase":{"kind":"running"}, "plan_id":id('2'), "chargeable_tokens":"7", "attempts":1,
        "history":[], "run_reports":[],
        "execution_records":[
            {"artifact_id":id('3'),"artifact_type":"af/TaskExecutionRecord@1","record":{"kind":"invocation","invocation_id":id('4')}},
            {"artifact_id":id('5'),"artifact_type":"af/TaskExecutionRecord@4","record":{"kind":"owned_children_registered","child_set_id":id('6')}}
        ],
        "owned_child_sets":[{"artifact_id":id('6'),"artifact_type":"af/TaskOwnedChildSet@1","record":{
            "plan_id":id('2'), "parent_invocation_id":id('4'), "source_artifact_id":id('7'),
            "children":[{"node":"root.scatter.slice1","source_item_id":id('8'),"invocation_id":id('9')}]
        }}]
    });
    assert_valid("task-inspection-v5.json", &value);
    for (pointer, bad_value) in [
        (
            "/execution_records/1/artifact_type",
            json!("af/TaskExecutionRecord@1"),
        ),
        (
            "/execution_records/0/artifact_type",
            json!("af/TaskExecutionRecord@4"),
        ),
        (
            "/owned_child_sets/0/artifact_type",
            json!("af/TaskOutput@1"),
        ),
        ("/chargeable_tokens", json!(7)),
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = bad_value;
        assert_invalid("task-inspection-v5.json", &bad, pointer);
    }
    let mut bad = value.clone();
    bad["owned_child_sets"][0]["record"]["children"][0]["allowance"] = json!(100);
    assert_invalid(
        "task-inspection-v5.json",
        &bad,
        "registry data cannot invent authority",
    );
    value["schema"] = json!("af/task-inspection@3");
    assert_invalid(
        "task-inspection-v3.json",
        &value,
        "old inspection remains frozen",
    );
}

#[test]
fn owned_registration_data_preserves_exact_items_without_granting_authority() {
    let set = TaskOwnedChildSetV1 {
        plan_id: id('a'),
        parent_invocation_id: id('b'),
        source_artifact_id: id('c'),
        children: vec![TaskOwnedChildV1 {
            node: "root.scatter.slice0".into(),
            source_item_id: id('d'),
            invocation_id: id('e'),
        }],
    };
    set.validate().unwrap();
    assert_eq!(
        set.artifact_refs(),
        vec![id('a'), id('b'), id('c'), id('d'), id('e')]
    );
    let value = serde_json::to_value(&set).unwrap();
    assert_valid("task-owned-child-set-v1.json", &value);
    for (pointer, invalid) in [
        ("/plan_id", json!("invented")),
        ("/children/0/node", json!("root.scatter#0")),
        ("/children/0/source_item_id", json!("slice:logical")),
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = invalid;
        assert_invalid("task-owned-child-set-v1.json", &bad, pointer);
        assert!(
            serde_json::from_value::<TaskOwnedChildSetV1>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut authority = value.clone();
    authority["children"][0]["max_attempts"] = json!(2);
    assert_invalid(
        "task-owned-child-set-v1.json",
        &authority,
        "data cannot carry authority",
    );
    assert!(serde_json::from_value::<TaskOwnedChildSetV1>(authority).is_err());
    let mut empty = set.clone();
    empty.children.clear();
    empty.validate().unwrap();
    assert_valid(
        "task-owned-child-set-v1.json",
        &serde_json::to_value(empty).unwrap(),
    );
    for duplicate in 0..3 {
        let mut bad = set.clone();
        let mut child = TaskOwnedChildV1 {
            node: "root.scatter.slice1".into(),
            source_item_id: id('f'),
            invocation_id: id('0'),
        };
        match duplicate {
            0 => child.node = set.children[0].node.clone(),
            1 => child.source_item_id = set.children[0].source_item_id.clone(),
            _ => child.invocation_id = set.children[0].invocation_id.clone(),
        };
        bad.children.push(child);
        assert!(bad.validate().is_err());
    }
}

#[test]
fn owned_lifecycle_requires_v4_and_cannot_be_smuggled_through_frozen_v1() {
    for record in [
        TaskExecutionRecordV4::OwnedChildrenRegistered {
            child_set_id: id('a'),
        },
        TaskExecutionRecordV4::OwnedChildPublished {
            child_set_id: id('a'),
            output_id: id('b'),
            attempt_id: "x".repeat(26),
        },
        TaskExecutionRecordV4::OwnedChildrenCompleted {
            child_set_id: id('a'),
            output_id: id('b'),
        },
    ] {
        record.validate().unwrap();
        let normalized = record.clone().into_record();
        assert_eq!(
            TaskExecutionRecordV4::from_owned(&normalized),
            Some(record.clone())
        );
        assert!(normalized.validate().is_err());
        assert!(serde_json::to_value(&normalized).is_err());
        let value = serde_json::to_value(record).unwrap();
        assert_valid("task-execution-record-v4.json", &value);
        assert_invalid("task-execution-record-v1.json", &value, "frozen v1");
        assert!(serde_json::from_value::<TaskExecutionRecordV1>(value.clone()).is_err());
        for invalid in [json!(null), json!("not-a-digest")] {
            let mut bad = value.clone();
            bad["child_set_id"] = invalid;
            assert_invalid(
                "task-execution-record-v4.json",
                &bad,
                "invalid child identity",
            );
            assert!(
                serde_json::from_value::<TaskExecutionRecordV4>(bad)
                    .map_or(true, |r| r.validate().is_err())
            );
        }
        let mut bad = value;
        bad["reserved_tokens"] = json!(1);
        assert_invalid(
            "task-execution-record-v4.json",
            &bad,
            "no reservation authority",
        );
        assert!(serde_json::from_value::<TaskExecutionRecordV4>(bad).is_err());
    }
    let legacy = TaskExecutionRecordV1::Invocation {
        invocation_id: id('a'),
    };
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        json!({"kind":"invocation","invocation_id":id('a')})
    );
    assert!(TaskExecutionRecordV4::from_owned(&legacy).is_none());
    assert_invalid(
        "task-execution-record-v4.json",
        &serde_json::to_value(legacy).unwrap(),
        "v4 is only owned lifecycle",
    );
}
