//! Broker and model tools share a single, versioned byte/pagination contract.
use af_contracts::navigation::{Request, Response, SCHEMA, tool_schemas};
use serde_json::{Value, json};

#[test]
fn navigation_wire_and_tool_parameters_admit_the_same_requests() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let tools = tool_schemas();
    for request in [
        json!({"tool":"af_list_paths","offset":1024}),
        json!({"tool":"af_read_range","path":"src/unicode","offset":2,"length":65536}),
        json!({"tool":"af_search","query":"needle","offset":128}),
    ] {
        assert!(validator.is_valid(&request));
        Request::parse(&serde_json::to_vec(&request).unwrap()).unwrap();
        let tool = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == request["tool"])
            .unwrap();
        let mut args = request.clone();
        args.as_object_mut().unwrap().remove("tool");
        assert!(
            jsonschema::validator_for(&tool["inputSchema"])
                .unwrap()
                .is_valid(&args)
        );
    }
    for request in [
        json!({"tool":"af_list_paths","prefix":"","limit":256}),
        json!({"tool":"af_read_range","path":"src/unicode","start_line":1,"line_count":10}),
        json!({"tool":"af_search","query":"needle"}),
        json!({"tool":"af_list_paths","offset":16385}),
        json!({"tool":"af_read_range","path":"../oracle","offset":0,"length":1}),
        json!({"tool":"af_search","query":"needle","offset":0,"limit":128}),
    ] {
        assert!(!validator.is_valid(&request));
        assert!(Request::parse(&serde_json::to_vec(&request).unwrap()).is_err());
    }
    assert!(Request::parse(br#"{"tool":"af_list_paths","offset":0,"offset":1}"#).is_err());
}

#[test]
fn navigation_responses_require_explicit_non_text_and_completeness_fields() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema["$defs"]["response"]).unwrap();
    for response in [
        json!({"tool":"af_read_range","text":null}),
        json!({"tool":"af_search","hits":[],"next_offset":null,"non_text_paths":["image.png"]}),
    ] {
        assert!(validator.is_valid(&response));
        let typed: Response = serde_json::from_value(response.clone()).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), response);
        for key in response.as_object().unwrap().keys() {
            let mut missing = response.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(!validator.is_valid(&missing));
            assert!(serde_json::from_value::<Response>(missing).is_err());
        }
    }
}
