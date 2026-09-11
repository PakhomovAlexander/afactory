//! Shared language-neutral fixture expansion and typed round-trips; no runtime admission.
use review_core::task::{
    TaskPhaseV1, TaskResultV1, TaskRevisionV1,
    pipeline::PipelineDefinitionV1,
    plan::{ExecutionPlanV1, PlanDecisionV1},
    review::{RepairAssessmentV1, ReviewHistoryV1, VerificationContinuationV1},
};
use serde_json::Value;

pub fn typed_round_trip(contract: &str, value: Value) -> Result<Value, String> {
    review_core::json::admit(&value).map_err(|e| e.to_string())?;
    macro_rules! check {
        ($ty:ty) => {{
            let typed: $ty = serde_json::from_value(value).map_err(|e| e.to_string())?;
            typed.validate()?;
            serde_json::to_value(typed).map_err(|e| e.to_string())
        }};
    }
    match contract {
        "task-revision" => check!(TaskRevisionV1),
        "pipeline-definition" => check!(PipelineDefinitionV1),
        "execution-plan" => check!(ExecutionPlanV1),
        "plan-decision" => check!(PlanDecisionV1),
        "task-result" => check!(TaskResultV1),
        "task-phase" => check!(TaskPhaseV1),
        "review-history" => check!(ReviewHistoryV1),
        "verification-continuation" => check!(VerificationContinuationV1),
        "repair-assessment" => check!(RepairAssessmentV1),
        _ => panic!("unregistered Task contract: {contract}"),
    }
}

pub fn expand(base: &Value, case: &Value) -> Value {
    let mut value = base.clone();
    let pointer = case["pointer"].as_str().expect("fixture JSON Pointer");
    let (parent, key) = pointer.rsplit_once('/').expect("non-root fixture mutation");
    let key = key.replace("~1", "/").replace("~0", "~");
    let parent = value
        .pointer_mut(parent)
        .expect("mutation parent must exist");
    match (case["op"].as_str().unwrap(), parent) {
        ("add", Value::Object(object)) => {
            assert!(!object.contains_key(&key), "add must name an absent field");
            object.insert(key, case["value"].clone());
        }
        ("replace", Value::Object(object)) => {
            assert!(
                object.contains_key(&key),
                "replace must name an existing field"
            );
            object.insert(key, case["value"].clone());
        }
        ("remove", Value::Object(object)) => {
            assert!(object.remove(&key).is_some());
        }
        ("replace", Value::Array(array)) => {
            array[key.parse::<usize>().unwrap()] = case["value"].clone();
        }
        _ => panic!("unsupported fixture mutation"),
    }
    assert_ne!(&value, base, "negative mutation must change its base");
    value
}
