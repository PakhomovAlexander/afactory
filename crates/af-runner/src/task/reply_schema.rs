//! Provider-native output schemas derived from a Task Worker's captured request (ADR-0103,
//! ADR-0208). Claude Code receives the transport schema inline as `--json-schema`; Codex
//! receives its strict projection through `--output-schema`. The kernel still enforces the
//! complete captured payload contracts, including every constraint omitted by that projection.

use serde_json::{Map, Value, json};

mod dialect;
pub use dialect::validate_strict;
use dialect::{allows_type, compatible_literals, has_type, scalar_type};

use super::{MAX_WORKER_BYTES, WORKER_REPLY_FORMAT, WorkerRequest};

/// The reply contract one native call is constrained to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyContract {
    /// The typed request's own captured output schemas (ADR-0103, ADR-0208). A captured legacy
    /// prompt has none and runs unconstrained.
    Request,
}

impl ReplyContract {
    /// The schema an inline facility (Claude Code `--json-schema`) receives: optional fields
    /// stay optional. None when the call runs unconstrained.
    pub fn transport(self, input: &[u8]) -> Result<Option<Value>, String> {
        match self {
            Self::Request => transport(input),
        }
    }

    /// The schema a strict facility (Codex `--output-schema`) receives: every property
    /// required, an optional one nullable. None when the call runs unconstrained.
    pub fn strict(self, input: &[u8]) -> Result<Option<Value>, String> {
        match self {
            Self::Request => strict(input),
        }
    }

    /// Remove what [`ReplyContract::strict`] made the model write and the contract never asked
    /// for. A reply that is not JSON is returned unchanged for the kernel to judge.
    pub fn normalize_strict_reply(self, input: &[u8], reply: &[u8]) -> Vec<u8> {
        match self {
            Self::Request => normalize_strict_reply(input, reply),
        }
    }
}

/// The typed request `input` renders, when it is one: None for a captured legacy prompt.
fn typed_request(input: &[u8]) -> Result<Option<WorkerRequest>, String> {
    if input.len() > MAX_WORKER_BYTES {
        return Err("Worker input exceeds byte bound".into());
    }
    let Ok(value) = serde_json::from_slice::<Value>(input) else {
        return Ok(None); // Captured legacy prompts keep their existing transport.
    };
    let Some(version) = value.get("schema").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !version.starts_with("af.worker-request/") {
        return Ok(None);
    }
    let request: WorkerRequest =
        serde_json::from_value(value).map_err(|e| format!("Invalid typed Worker request: {e}"))?;
    if request.schema != "af.worker-request/1"
        || request.reply_format != WORKER_REPLY_FORMAT
        || request.output_schemas.is_empty()
        || request.output_schemas.len() > 32
        || request
            .output_schemas
            .keys()
            .any(|p| !af_contracts::task::is_name(p))
    {
        return Err("Unsupported typed Worker request contract".into());
    }
    Ok(Some(request))
}

/// The reply envelope of ADR-0103 around the exact captured payload schemas: `schema` is
/// `af.worker-reply/1`, `outputs` holds the declared ports, each an array of at most 1,024
/// payloads; unknown outer fields and ports are forbidden. Each object payload schema gets a
/// transport-only `$id` that keeps its local references scoped to it once nested.
pub fn transport(input: &[u8]) -> Result<Option<Value>, String> {
    let Some(request) = typed_request(input)? else {
        return Ok(None);
    };
    let mut properties = Map::new();
    for (index, (port, mut payload)) in request.output_schemas.into_iter().enumerate() {
        match &mut payload {
            Value::Object(schema) if !schema.contains_key("$id") => {
                schema.insert(
                    "$id".into(),
                    json!(format!("urn:afactory:worker-output:{index}")),
                );
            }
            Value::Bool(_) => {}
            _ => return Err("Invalid output payload schema".into()),
        }
        properties.insert(
            port,
            json!({"type":"array","maxItems":1024,"items":payload}),
        );
    }
    Ok(Some(json!({
        "type":"object", "additionalProperties":false, "required":["schema","outputs"],
        "properties":{
            "schema":{"type":"string","const":"af.worker-reply/1"},
            "outputs":{"type":"object","additionalProperties":false,"properties":properties}
        }
    })))
}

/// Plan-time preflight and invocation share the exact whole output-port projection.
/// Unsupported contracts deliberately retain kernel-only validation. Invalid contracts refuse.
pub fn strict_outputs(
    schemas: &std::collections::BTreeMap<String, Value>,
) -> Result<Option<Value>, String> {
    // Check every port before deciding unsupported mode; a reference in one port must not
    // hide a malformed schema in another.
    for (port, payload) in schemas {
        super::schema_validator(payload)
            .map_err(|error| format!("Worker output port {port}: {error}"))?;
    }
    let mut properties = Map::new();
    for (port, payload) in schemas {
        let Some(payload) = project(payload, 0) else {
            return Ok(None);
        };
        properties.insert(port.clone(), json!({"type":"array","items":payload}));
    }
    let schema = json!({
        "type":"object", "additionalProperties":false, "required":["schema","outputs"],
        "properties":{
            "schema":{"type":"string","enum":["af.worker-reply/1"]},
            "outputs":{
                "type":"object", "additionalProperties":false,
                "required": properties.keys().cloned().map(Value::String).collect::<Vec<_>>(),
                "properties":properties
            }
        }
    });
    validate_strict(&schema)?;
    Ok(Some(schema))
}

/// The keywords a strict native schema keeps; every other one is the kernel's alone to apply.
const STRICT_KEYWORDS: [&str; 8] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "anyOf",
];

/// The structural projection of one captured payload schema for a strict native facility, which
/// requires every object to name all its properties as required and to admit no other: an
/// optional property becomes required and nullable, `oneOf` becomes `anyOf`, and every keyword
/// a strict facility does not take (`allOf`, `not`, `if`, string and number bounds, …) is left to
/// the kernel. None when the schema cannot be projected: a boolean schema, an open map, a
/// reference, or an untyped schema without inferable scalar literals.
fn project(schema: &Value, depth: usize) -> Option<Value> {
    if depth > 32 {
        return None;
    }
    if !compatible_literals(schema) {
        return None;
    }
    let schema = schema.as_object()?;
    if schema.contains_key("$ref")
        || schema.contains_key("$defs")
        || schema.contains_key("patternProperties")
    {
        return None;
    }
    // Literal values are data, never schema nodes (including object keys named `$ref`).
    if schema.contains_key("oneOf") && schema.contains_key("anyOf") {
        return None;
    }
    let mut schema = schema.clone();
    let literals = if let Some(value) = schema.get("const") {
        Some(vec![value.clone()])
    } else {
        schema.get("enum").and_then(Value::as_array).cloned()
    };
    if let Some(values) = literals {
        let values: Vec<_> = values
            .into_iter()
            .filter(|value| {
                schema
                    .get("type")
                    .is_none_or(|types| allows_type(types, value))
            })
            .collect();
        if values.iter().any(|v| scalar_type(v).is_none()) {
            return None;
        }
        if values.is_empty() {
            return None;
        }
        if !schema.contains_key("type") {
            let mut types = Vec::new();
            for value in &values {
                let kind = scalar_type(value)?;
                if !types.contains(&kind) {
                    types.push(kind);
                }
            }
            schema.insert(
                "type".into(),
                if types.len() == 1 {
                    json!(types[0])
                } else {
                    json!(types)
                },
            );
        }
        schema.remove("const");
        schema.insert("enum".into(), json!(values));
    }
    // Object/array keywords alone do not exclude scalar instances. In particular, an
    // untyped union branch must not acquire an implicit object type. Scalar literal
    // inference above is safe because the literal restriction excludes other types.
    if !schema.contains_key("type")
        && ["properties", "required", "additionalProperties", "items"]
            .iter()
            .any(|key| schema.contains_key(*key))
    {
        return None;
    }
    // Required/nullable rewriting cannot compose independently across sibling structural
    // constraints and union branches. A closed enclosing object may require a synthetic
    // field that a closed branch forbids; array items can contain the same conflict.
    // Preserve the complete request in kernel-only mode rather than narrow its solutions.
    if (schema.contains_key("anyOf") || schema.contains_key("oneOf"))
        && (has_type(schema.get("type"), "object") || has_type(schema.get("type"), "array"))
    {
        return None;
    }
    let mut projected = Map::new();
    for (key, value) in &schema {
        let key = if key == "oneOf" {
            "anyOf"
        } else {
            key.as_str()
        };
        if !STRICT_KEYWORDS.contains(&key) {
            continue;
        }
        let value = match key {
            "properties" | "required" | "additionalProperties" => continue,
            "items" => {
                if schema.contains_key("type") && !has_type(schema.get("type"), "array") {
                    continue;
                }
                project(value, depth + 1)?
            }
            "anyOf" => Value::Array(
                value
                    .as_array()?
                    .iter()
                    .map(|branch| project(branch, depth + 1))
                    .collect::<Option<_>>()?,
            ),
            _ => value.clone(),
        };
        projected.insert(key.into(), value);
    }
    let is_object = has_type(schema.get("type"), "object");
    if is_object {
        // A strict object is closed: an open map has no strict form.
        if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
            return None;
        }
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut properties = Map::new();
        if let Some(declared) = schema.get("properties") {
            let all_required = closed_required(&schema);
            for (name, property) in declared.as_object()? {
                let property = project(property, depth + 1)?;
                properties.insert(
                    name.clone(),
                    if all_required || required.contains(&name.as_str()) {
                        property
                    } else {
                        json!({"anyOf":[property, {"type":"null"}]})
                    },
                );
            }
        }
        projected.entry("type").or_insert(json!("object"));
        projected.insert(
            "required".into(),
            Value::Array(properties.keys().cloned().map(Value::String).collect()),
        );
        projected.insert("properties".into(), Value::Object(properties));
        projected.insert("additionalProperties".into(), Value::Bool(false));
    }
    if has_type(projected.get("type"), "array") && !projected.contains_key("items") {
        return None;
    }
    if !projected.contains_key("type") && !projected.contains_key("anyOf") {
        return None;
    }
    Some(Value::Object(projected))
}

/// In a closed object with N declared properties, minProperties=N requires every one.
fn closed_required(schema: &Map<String, Value>) -> bool {
    let Some(minimum) = schema.get("minProperties").and_then(Value::as_u64) else {
        return false;
    };
    schema.get("additionalProperties") == Some(&Value::Bool(false))
        && !schema.contains_key("patternProperties")
        && schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| minimum == properties.len() as u64)
}

/// The reply envelope for a strict native facility (Codex `--output-schema`): the envelope of
/// [`transport`] with every declared port required — an unused port is an empty array, which
/// [`normalize_strict_reply`] removes — around the structural projection of each payload schema.
/// None when the request is a legacy prompt or a payload schema has no strict projection; the
/// call then runs unconstrained, exactly as before, and the kernel alone judges its reply.
pub fn strict(input: &[u8]) -> Result<Option<Value>, String> {
    let Some(request) = typed_request(input)? else {
        return Ok(None);
    };
    strict_outputs(&request.output_schemas)
}

/// Remove what a strict facility made the model write and the captured contract never asked
/// for: `null` for an optional property, and an empty array for an unused port. Anything else,
/// and a reply that is not JSON, is returned unchanged for the kernel to judge.
pub fn normalize_strict_reply(input: &[u8], reply: &[u8]) -> Vec<u8> {
    let Ok(Some(request)) = typed_request(input) else {
        return reply.to_vec();
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(reply) else {
        return reply.to_vec();
    };
    let Some(outputs) = value.get_mut("outputs").and_then(Value::as_object_mut) else {
        return reply.to_vec();
    };
    outputs.retain(|port, items| {
        !request.output_schemas.contains_key(port)
            || items.as_array().is_none_or(|items| !items.is_empty())
    });
    for (port, items) in outputs.iter_mut() {
        let (Some(schema), Some(items)) = (request.output_schemas.get(port), items.as_array_mut())
        else {
            continue;
        };
        for item in items {
            let mut normalized = item.clone();
            if strip_optional_nulls(&mut normalized, schema, 0) {
                *item = normalized;
            }
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| reply.to_vec())
}

fn strip_optional_nulls(value: &mut Value, schema: &Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    // Preserve original valid values before attempting synthetic-null removal in a union.
    if super::schema_validator(schema).is_ok_and(|v| v.is_valid(value)) {
        return true;
    }
    if let (Some(object), Some(properties)) = (
        value.as_object_mut(),
        schema.get("properties").and_then(Value::as_object),
    ) {
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let all_required = schema.as_object().is_some_and(closed_required);
        object.retain(|name, field| {
            !field.is_null()
                || required.contains(&name.as_str())
                || all_required && properties.contains_key(name)
                || properties.get(name).is_none_or(|schema| {
                    super::schema_validator(schema).is_ok_and(|v| v.is_valid(&Value::Null))
                })
        });
        for (name, field) in object.iter_mut() {
            if let Some(property) = properties.get(name)
                && !strip_optional_nulls(field, property, depth + 1)
            {
                return false;
            }
        }
    }
    if let (Some(items), Some(item_schema)) = (value.as_array_mut(), schema.get("items")) {
        for item in items {
            if !strip_optional_nulls(item, item_schema, depth + 1) {
                return false;
            }
        }
    }
    if super::schema_validator(schema).is_ok_and(|v| v.is_valid(value)) {
        return true;
    }
    // Normalize siblings first; a branch may only remove nulls when the complete
    // enclosing contract accepts the result. Prefer the valid candidate retaining the
    // most nulls: an earlier alternative must not erase a later one's legal nulls.
    fn nulls(value: &Value) -> usize {
        match value {
            Value::Null => 1,
            Value::Array(items) => items.iter().map(nulls).sum(),
            Value::Object(fields) => fields.values().map(nulls).sum(),
            _ => 0,
        }
    }
    let mut best = None;
    for keyword in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                let mut normalized = value.clone();
                if !strip_optional_nulls(&mut normalized, branch, depth + 1) {
                    return false;
                }
                let retained = nulls(&normalized);
                if super::schema_validator(schema).is_ok_and(|v| v.is_valid(&normalized))
                    && best.as_ref().is_none_or(|(count, _)| retained > *count)
                {
                    best = Some((retained, normalized));
                }
            }
        }
    }
    if let Some((_, normalized)) = best {
        *value = normalized;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request(schemas: BTreeMap<String, Value>) -> Vec<u8> {
        WorkerRequest::new("Answer.", BTreeMap::new(), Vec::new(), schemas)
            .rendered()
            .unwrap()
    }

    #[test]
    fn a_closed_contract_projects_with_optional_fields_nullable_and_every_port_required() {
        let schemas = BTreeMap::from([
            (
                "result".to_string(),
                json!({"type":"object","additionalProperties":false,"required":["outcome"],
                    "properties":{"outcome":{"enum":["passed","failed"]},
                        "note":{"type":"string","minLength":1,"maxLength":10}},
                    "allOf":[{"if":{"properties":{"outcome":{"const":"failed"}}},"then":{"required":["note"]}}]}),
            ),
            (
                "notes".to_string(),
                json!({"type":"object","additionalProperties":false,"properties":{}}),
            ),
        ]);
        let strict = strict(&request(schemas.clone())).unwrap().unwrap();
        assert_eq!(
            strict["properties"]["outputs"]["required"],
            json!(["notes", "result"])
        );
        let result = &strict["properties"]["outputs"]["properties"]["result"]["items"];
        assert_eq!(result["required"], json!(["outcome", "note"]));
        assert_eq!(
            result["properties"]["note"],
            json!({"anyOf":[{"type":"string"},{"type":"null"}]})
        );
        assert!(result.get("allOf").is_none(), "left to the kernel");
        assert_eq!(result["additionalProperties"], json!(false));
        // What the strict facility made the model write, normalized back to the contract.
        let normalized = normalize_strict_reply(
            &request(schemas),
            br#"{"schema":"af.worker-reply/1","outputs":{"notes":[],"result":[{"outcome":"passed","note":null}]}}"#,
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&normalized).unwrap(),
            json!({"schema":"af.worker-reply/1","outputs":{"result":[{"outcome":"passed"}]}})
        );
    }

    #[test]
    fn an_open_map_or_a_reference_has_no_strict_projection() {
        for payload in [
            json!({"type":"object","additionalProperties":{"type":"string"}}),
            json!({"type":"object","properties":{"a":{"$ref":"#/$defs/a"}},"$defs":{"a":{"type":"string"}},"additionalProperties":false}),
            json!(true),
        ] {
            let schemas = BTreeMap::from([("result".to_string(), payload)]);
            assert_eq!(strict(&request(schemas)).unwrap(), None);
        }
        assert_eq!(strict(b"a legacy prompt").unwrap(), None);
    }

    #[test]
    fn closed_property_count_keeps_required_nullable_fields_and_optional_fields_stay_optional() {
        let schema = json!({"type":"object","additionalProperties":false,"minProperties":1,
            "properties":{"nested":{"type":"object","additionalProperties":false,"minProperties":1,
                "properties":{"value":{"type":["string","null"]}}}}});
        let input = request(BTreeMap::from([("result".into(), schema.clone())]));
        let projection = strict(&input).unwrap().unwrap();
        let payload = &projection["properties"]["outputs"]["properties"]["result"]["items"];
        assert_eq!(
            payload["properties"]["nested"]["properties"]["value"],
            json!({"type":["string","null"]})
        );
        let reply =
            json!({"schema":"af.worker-reply/1","outputs":{"result":[{"nested":{"value":null}}]}});
        assert_eq!(
            serde_json::from_slice::<Value>(&normalize_strict_reply(
                &input,
                &serde_json::to_vec(&reply).unwrap()
            ))
            .unwrap(),
            reply
        );

        for change in ["lower-count", "no-count", "pattern-properties"] {
            let mut optional = schema.clone();
            match change {
                "lower-count" => optional["minProperties"] = json!(0),
                "no-count" => {
                    optional.as_object_mut().unwrap().remove("minProperties");
                }
                _ => optional["patternProperties"] = json!({"^other$":{"type":"string"}}),
            }
            let input = request(BTreeMap::from([("result".into(), optional)]));
            if change == "pattern-properties" {
                // Main's strict projection refuses patternProperties rather than narrowing it.
                assert_eq!(strict(&input).unwrap(), None);
                continue;
            }
            let projection = strict(&input).unwrap().unwrap();
            let nested = &projection["properties"]["outputs"]["properties"]["result"]["items"]["properties"]
                ["nested"];
            assert_eq!(nested["anyOf"][1], json!({"type":"null"}), "{change}");
        }
    }

    #[test]
    fn the_transport_wrapper_scopes_each_payload_and_bounds_its_items() {
        let schemas = BTreeMap::from([(
            "result".to_string(),
            json!({"type":"object","additionalProperties":false,"properties":{}}),
        )]);
        let wrapper = transport(&request(schemas)).unwrap().unwrap();
        let port = &wrapper["properties"]["outputs"]["properties"]["result"];
        assert_eq!(port["maxItems"], 1024);
        assert_eq!(port["items"]["$id"], "urn:afactory:worker-output:0");
    }
    #[test]
    fn document_draft_v3_projects_into_the_real_strict_reply_envelope() {
        let mut document_schema: Value =
            serde_json::from_str(include_str!("../../../../schemas/document-draft-v3.json"))
                .unwrap();
        assert_eq!(
            document_schema["properties"]["schema"],
            json!({"const":"af.document-draft/3"}),
            "the public schema deliberately leaves the scalar literal untyped"
        );
        document_schema.as_object_mut().unwrap().remove("$id");
        document_schema.as_object_mut().unwrap().remove("$schema");
        let input = request(BTreeMap::from([("draft".into(), document_schema.clone())]));
        let projected = strict(&input).unwrap().unwrap();
        validate_strict(&projected).unwrap();
        assert_eq!(
            projected["properties"]["outputs"]["properties"]["draft"]["items"]["properties"]["schema"],
            json!({"type":"string","enum":["af.document-draft/3"]})
        );

        let draft = json!({
            "schema":"af.document-draft/3",
            "title":"Native schema projection",
            "sections":[{"heading":"Decision","body":"Infer string for the untyped draft constant."}],
            "citations":[
                {"kind":"source","name":"requirements"},
                {"kind":"repository","location":{
                    "path":"crates/af-runner/src/task/reply_schema.rs","line":1
                }}
            ]
        });
        assert!(
            super::super::schema_validator(&document_schema)
                .unwrap()
                .is_valid(&draft)
        );
        let reply = json!({"schema":"af.worker-reply/1","outputs":{"draft":[draft]}});
        assert!(
            jsonschema::draft202012::options()
                .build(&projected)
                .unwrap()
                .is_valid(&reply)
        );
    }

    #[test]
    fn scalar_literal_matrix_keeps_types_values_and_explicit_restrictions() {
        for (payload, expected_type, expected_enum) in [
            (
                json!({"const":"custom.issue-triage/2"}),
                json!("string"),
                json!(["custom.issue-triage/2"]),
            ),
            (
                json!({"enum":[null,"bug","perf"]}),
                json!(["null", "string"]),
                json!([null, "bug", "perf"]),
            ),
            (
                json!({"enum":[null,true,1,"x"]}),
                json!(["null", "boolean", "number", "string"]),
                json!([null, true, 1, "x"]),
            ),
            (
                json!({"type":"integer","enum":[1,1.5,"1",null]}),
                json!("integer"),
                json!([1]),
            ),
            (
                json!({"type":["string","null"],"enum":[null,"x",4]}),
                json!(["string", "null"]),
                json!([null, "x"]),
            ),
        ] {
            let projected = strict(&request(BTreeMap::from([("result".into(), payload)])))
                .unwrap()
                .unwrap();
            let leaf = &projected["properties"]["outputs"]["properties"]["result"]["items"];
            assert_eq!(leaf["type"], expected_type);
            assert_eq!(leaf["enum"], expected_enum);
            validate_strict(&projected).unwrap();
        }
    }

    #[test]
    fn local_reference_targets_obey_shared_bounds_and_literal_boundaries() {
        use super::super::schema_validator;
        for location in ["default", "const", "enum/0"] {
            for target in [
                json!({"$id":"https://example.test/other","type":"string"}),
                json!({"$ref":"https://example.test/other"}),
            ] {
                let mut schema = json!({"default":target,"const":target,"enum":[target]});
                schema["$ref"] = json!(format!("#/{location}"));
                assert!(schema_validator(&schema).is_err(), "{schema}");
                schema.as_object_mut().unwrap().remove("$ref");
                schema_validator(&schema).unwrap();
            }
        }
        let cycle = json!({"$ref":"#/default","default":{"$ref":"#/default"}});
        schema_validator(&cycle).unwrap();
        let mut deep = json!({"type":"string"});
        for _ in 0..65 {
            deep = json!({"items":deep});
        }
        assert!(schema_validator(&json!({"$ref":"#/default","default":deep})).is_err());
        let refs: Vec<_> = (0..8200).map(|_| json!({"$ref":"#/default"})).collect();
        assert!(schema_validator(&json!({"allOf":refs,"default":{"type":"string"}})).is_err());
        schema_validator(&json!({"allOf":[{"$ref":"#/default"},{"$ref":"#/default"}],"default":{"type":"string"}})).unwrap();
    }

    #[test]
    fn local_reference_fragments_match_kernel_resolution_before_admission() {
        use super::super::schema_validator;
        for (reference, name) in [
            ("#/a%20b", "a b"),
            ("#/%D1%81%D1%85%D0%B5%D0%BC%D0%B0", "схема"),
            ("#/%7e0", "~"),
            ("#/%7E1", "/"),
            ("#/%7E01", "~1"),
            ("#/a%2520b", "a%20b"),
        ] {
            let mut schema = json!({"$ref":reference});
            schema[name] = json!({"type":"string"});
            let validator = schema_validator(&schema).unwrap();
            assert!(validator.is_valid(&json!("ok")), "{schema}");
            assert!(!validator.is_valid(&json!(1)), "{schema}");
            // The encoded-name decoy must not hide a forbidden real target.
            schema[&reference[2..]] = json!({"type":"integer"});
            for forbidden in [
                json!({"$id":"https://example.test/other","type":"string"}),
                json!({"$ref":"https://example.test/other"}),
            ] {
                schema[name] = forbidden;
                assert!(schema_validator(&schema).is_err(), "{schema}");
            }
        }
        // Decode separators before pointer splitting, and resolve array indexes exactly
        // as referencing does (its usize parser also accepts leading zeros).
        for reference in ["#/default%2F0", "#/default/00"] {
            let schema = json!({"$ref":reference,"default":[{"type":"string"}]});
            assert!(schema_validator(&schema).unwrap().is_valid(&json!("ok")));
        }
        for fragment in ["%", "%2", "%GG", "%FF", "%C3%28"] {
            let reference = format!("#/{fragment}");
            let mut schema = json!({"$ref":reference});
            schema[fragment] = json!({"type":"string"});
            // Invalid URI escapes / non-UTF8 fragments fail under the pinned library;
            // a raw encoded-key decoy never authorizes a different decoded target.
            assert!(jsonschema::draft202012::options().build(&schema).is_err());
            assert!(schema_validator(&schema).is_err());
        }
        for reference in ["#anchor", "https://example.test/schema", "file:///schema"] {
            assert!(schema_validator(&json!({"$ref":reference})).is_err());
        }
    }

    #[test]
    fn literal_contradictions_use_kernel_numeric_equality() {
        for (value, member, projected) in [
            (json!(1), json!(1.0), true),
            (json!(1.0), json!(1), true),
            (json!(-0.0), json!(0), true),
            (json!([1, {"n": 2}]), json!([1.0, {"n": 2.0}]), false),
            (json!({"n": [1]}), json!({"n": [1.0]}), false),
        ] {
            let payload = json!({"const":value,"enum":[member]});
            let validator = super::super::schema_validator(&payload).unwrap();
            assert!(validator.is_valid(&value), "{payload}");
            assert!(validator.is_valid(&member), "{payload}");
            let result = strict(&request(BTreeMap::from([("result".into(), payload)]))).unwrap();
            assert_eq!(result.is_some(), projected);
            if let Some(schema) = result {
                validate_strict(&schema).unwrap();
                let leaf = &schema["properties"]["outputs"]["properties"]["result"]["items"];
                let projected_validator = super::super::schema_validator(leaf).unwrap();
                assert!(projected_validator.is_valid(&value));
                assert!(projected_validator.is_valid(&member));
            }
        }
        for (value, member) in [
            (json!(1), json!(1.5)),
            (json!(9007199254740993_u64), json!(9007199254740992_u64)),
            (json!([1, {"n": 2}]), json!([1.0, {"n": 3.0}])),
            (json!({"n": [1]}), json!({"n": [2.0]})),
        ] {
            let payload = json!({"const":value,"enum":[member]});
            assert!(
                !super::super::schema_validator(&payload)
                    .unwrap()
                    .is_valid(&value)
            );
            assert!(
                strict(&request(BTreeMap::from([("result".into(), payload)])))
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn invalid_unsupported_and_literal_objects_are_distinct_for_the_whole_request() {
        for invalid in [json!({"type":"bogus"}), json!({"$ref":"#/$defs/missing"})] {
            let outputs =
                BTreeMap::from([("a_open".into(), json!(true)), ("z_bad".into(), invalid)]);
            assert!(strict(&request(outputs)).unwrap_err().contains("z_bad"));
        }
        for literal in [
            json!({"enum":[]}),
            json!({"const":{"$ref":"https://invalid.test","type":"bogus"}}),
            json!({"enum":[{"$id":"literal","properties":4}]}),
        ] {
            // This is a valid kernel schema with literal data, intentionally unsupported by
            // scalar projection. Neither validator resolves or interprets the object's keys.
            super::super::schema_validator(&literal).unwrap();
            assert_eq!(
                strict(&request(BTreeMap::from([("result".into(), literal)]))).unwrap(),
                None
            );
        }
        for (payload, instance) in [
            (
                json!({"anyOf":[{"type":"string","const":1},{"type":"string"}]}),
                json!("ok"),
            ),
            (
                json!({"type":"object","additionalProperties":false,"properties":{"optional":{"type":"string","const":1}}}),
                json!({}),
            ),
            (
                json!({"type":"object","properties":{"name":{"type":"string"}}}),
                json!({"name":"x","extra":1}),
            ),
        ] {
            assert!(
                super::super::schema_validator(&payload)
                    .unwrap()
                    .is_valid(&instance)
            );
            assert!(
                strict_outputs(&BTreeMap::from([
                    ("result".into(), payload),
                    ("other".into(), json!({"type":"string"})),
                ]))
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn untyped_structural_keywords_keep_whole_request_kernel_only() {
        for untyped in [
            json!({"properties":{"name":{"type":"string"}},"additionalProperties":false}),
            json!({"required":["name"],"additionalProperties":false}),
            json!({"additionalProperties":false}),
            json!({"properties":{}}),
            json!({"items":{"type":"string"}}),
        ] {
            for payload in [
                untyped.clone(),
                json!({"anyOf":[untyped.clone(),{"type":"boolean"}]}),
                json!({"oneOf":[untyped.clone(),{"type":"boolean"}]}),
                json!({"anyOf":[{"type":"boolean"},{"anyOf":[untyped]}]}),
            ] {
                let outputs = BTreeMap::from([
                    ("result".into(), payload.clone()),
                    ("other".into(), json!({"type":"string"})),
                ]);
                let input = request(outputs.clone());
                let reply = json!({"schema":"af.worker-reply/1","outputs":{"result":["done"]}});
                assert!(
                    super::super::schema_validator(&payload)
                        .unwrap()
                        .is_valid(&json!("done"))
                );
                let transport = transport(&input).unwrap().unwrap();
                assert!(
                    jsonschema::validator_for(&transport)
                        .unwrap()
                        .is_valid(&reply)
                );
                assert_eq!(strict_outputs(&outputs).unwrap(), None, "{payload}");
                assert_eq!(strict(&input).unwrap(), None, "{payload}");
            }
        }
        // Explicit object types still project, and scalar literals may safely infer a type
        // even when irrelevant structural keywords are present.
        for payload in [
            json!({"type":"object","properties":{"name":{"type":"string"}},"additionalProperties":false}),
            json!({"const":"done","properties":{}}),
            json!({"enum":["done",null],"items":{"type":"string"}}),
        ] {
            let input = request(BTreeMap::from([("result".into(), payload)]));
            validate_strict(&strict(&input).unwrap().unwrap()).unwrap();
        }
    }

    #[test]
    fn emitted_dialect_validator_rejects_malformed_structures_with_paths_and_bounds() {
        for bad in [
            json!({"const":"x"}),
            json!({"type":"array"}),
            json!({"type":"object","properties":{},"required":[]}),
            json!({"anyOf":[]}),
            json!({"type":"string","enum":[4]}),
            json!({"type":["null","null"]}),
            json!({"type":"number","minimum":0}),
        ] {
            assert!(
                validate_strict(&bad)
                    .unwrap_err()
                    .starts_with("Strict schema ")
            );
        }
        let mut nested = json!({"type":"string"});
        for _ in 0..34 {
            nested = json!({"type":"array","items":nested});
        }
        assert!(validate_strict(&nested).unwrap_err().contains("bound"));
    }

    #[test]
    fn nested_nullable_arrays_unions_preserve_null_and_kernel_constraints() {
        let payload = json!({"type":"object","additionalProperties":false,"required":["items"],"properties":{
            "nullable":{"enum":[null,"bug"]},
            "items":{"type":["array","null"],"items":{"oneOf":[
                {"type":"object","required":["tag"],"additionalProperties":false,"properties":{"tag":{"const":"a"},"note":{"type":"string"}}},
                {"type":"null"}
            ]}}
        }});
        let input = request(BTreeMap::from([("result".into(), payload)]));
        let projected = strict(&input).unwrap().unwrap();
        validate_strict(&projected).unwrap();
        let normalized = normalize_strict_reply(&input, br#"{"schema":"af.worker-reply/1","outputs":{"result":[{"nullable":null,"items":[{"tag":"a","note":null},null]}]}}"#);
        assert_eq!(
            serde_json::from_slice::<Value>(&normalized).unwrap()["outputs"]["result"][0],
            json!({"nullable":null,"items":[{"tag":"a"},null]})
        );
    }
    #[test]
    fn depth_cutoff_discards_partial_payload_cleanup() {
        let mut nested_schema = json!({"type":"string"});
        let mut nested_value = json!(null);
        for _ in 0..34 {
            nested_schema = json!({"type":"object","properties":{"child":nested_schema},"required":["child"],"additionalProperties":false});
            nested_value = json!({"child":nested_value});
        }
        let payload = json!({"type":"object","properties":{
            "optional":{"type":"string"}, "nested":nested_schema
        },"required":["nested"],"additionalProperties":false});
        let input = request(BTreeMap::from([("result".into(), payload)]));
        let reply = json!({"schema":"af.worker-reply/1","outputs":{"result":[
            {"optional":null,"nested":nested_value}
        ]}});
        // Projection declines this depth, so exercise the bounded normalizer directly
        // through its typed-reply API, independently of projection admission.
        assert_eq!(strict(&input).unwrap(), None);
        let normalized: Value = serde_json::from_slice(&normalize_strict_reply(
            &input,
            &serde_json::to_vec(&reply).unwrap(),
        ))
        .unwrap();
        assert_eq!(normalized, reply);
    }

    #[test]
    fn union_normalization_respects_enclosing_required_nullable_fields() {
        for required in [json!(["a"]), json!([])] {
            let payload = json!({"type":"object","required":required,
            "properties":{"a":{"type":["string","null"]},"b":{"type":"string"}},
            "additionalProperties":false,"anyOf":[
                {"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},"additionalProperties":false},
                {"type":"object","required":["a"],"properties":{"a":{"type":"null"},"b":{"type":"string"}},"additionalProperties":false}
            ]});
            let input = request(BTreeMap::from([("result".into(), payload.clone())]));
            let reply =
                json!({"schema":"af.worker-reply/1","outputs":{"result":[{"a":null,"b":null}]}});
            // Sibling object/union rewriting is deliberately unsupported. Keep testing
            // normalization independently against the complete original kernel contract.
            assert_eq!(strict(&input).unwrap(), None);
            let normalized: Value = serde_json::from_slice(&normalize_strict_reply(
                &input,
                &serde_json::to_vec(&reply).unwrap(),
            ))
            .unwrap();
            let item = &normalized["outputs"]["result"][0];
            assert_eq!(item, &json!({"a":null}));
            assert!(
                super::super::schema_validator(&payload)
                    .unwrap()
                    .is_valid(item)
            );
        }
    }

    #[test]
    fn union_normalization_preserves_nullable_values_declared_only_in_alternatives() {
        for keyword in ["anyOf", "oneOf"] {
            for reverse in [false, true] {
                let mut branches = vec![
                    json!({"type":"object","additionalProperties":false,
                        "properties":{"a":{"type":"string"},"b":{"type":"string"}}}),
                    json!({"type":"object","additionalProperties":false,
                        "properties":{"a":{"type":"null"},"b":{"type":"string"}}}),
                ];
                if reverse {
                    branches.reverse();
                }
                let payload = json!({keyword:branches});
                // Exercise both direct union payloads and nested array items.
                for nested in [false, true] {
                    let schema = if nested {
                        json!({"type":"array","items":payload})
                    } else {
                        payload.clone()
                    };
                    let input = request(BTreeMap::from([("result".into(), schema.clone())]));
                    let item = if nested {
                        json!([{"a":null,"b":null}])
                    } else {
                        json!({"a":null,"b":null})
                    };
                    let reply = json!({"schema":"af.worker-reply/1","outputs":{"result":[item]}});
                    assert!(
                        super::super::schema_validator(&strict(&input).unwrap().unwrap())
                            .unwrap()
                            .is_valid(&reply)
                    );
                    let normalized: Value = serde_json::from_slice(&normalize_strict_reply(
                        &input,
                        &serde_json::to_vec(&reply).unwrap(),
                    ))
                    .unwrap();
                    let item = &normalized["outputs"]["result"][0];
                    let expected = if nested {
                        json!([{"a":null}])
                    } else {
                        json!({"a":null})
                    };
                    assert_eq!(item, &expected, "{keyword}, reverse={reverse}");
                    assert!(
                        super::super::schema_validator(&schema)
                            .unwrap()
                            .is_valid(item)
                    );
                }
            }
        }
    }

    #[test]
    fn pattern_properties_keep_explicit_unsupported_projection() {
        let payload = json!({"type":"object","patternProperties":{"^x":{"type":"string"}},
            "additionalProperties":false,"required":["x"]});
        assert!(
            super::super::schema_validator(&payload)
                .unwrap()
                .is_valid(&json!({"x":"ok"}))
        );
        assert_eq!(
            strict(&request(BTreeMap::from([("result".into(), payload)]))).unwrap(),
            None
        );
    }

    #[test]
    fn sibling_structural_unions_keep_whole_request_kernel_only() {
        for keyword in ["anyOf", "oneOf"] {
            let branch_a = json!({"type":"object","properties":{"a":{"type":"string"}},
                "additionalProperties":false});
            let branch_b = json!({"type":"object","properties":{"b":{"type":"string"}},
                "additionalProperties":false});
            let mut object = json!({"type":"object","additionalProperties":false,
                "properties":{"a":{"type":"string"},"b":{"type":"string"}}});
            object[keyword] = json!([branch_a, branch_b]);
            let mut nullable = object.clone();
            nullable["type"] = json!(["object", "null"]);
            let mut array = json!({"type":"array","items":{
                "type":"object","additionalProperties":false,
                "properties":{"a":{"type":"string"},"b":{"type":"string"}}}});
            array[keyword] = json!([
                {"type":"array","items":branch_a},
                {"type":"array","items":branch_b}
            ]);
            let mut nullable_array = array.clone();
            nullable_array["type"] = json!(["array", "null"]);
            for (payload, instance) in [
                (object.clone(), json!({"a":"x"})),
                (nullable, json!({"b":"x"})),
                (array, json!([{"a":"x"}])),
                (nullable_array, json!([{"b":"x"}])),
                (
                    json!({"type":"array","items":object.clone()}),
                    json!([{"a":"x"}]),
                ),
                (
                    json!({"type":"object","additionalProperties":false,"required":["nested"],
                    "properties":{"nested":object.clone()}}),
                    json!({"nested":{"b":"x"}}),
                ),
                (
                    json!({"anyOf":[object,{"type":"boolean"}]}),
                    json!({"a":"x"}),
                ),
            ] {
                assert!(
                    super::super::schema_validator(&payload)
                        .unwrap()
                        .is_valid(&instance),
                    "{payload}: {instance}"
                );
                let outputs = BTreeMap::from([
                    ("result".into(), payload.clone()),
                    ("other".into(), json!({"const":"safe"})),
                ]);
                let input = request(outputs.clone());
                let reply = json!({"schema":"af.worker-reply/1","outputs":{"result":[instance]}});
                assert!(
                    jsonschema::validator_for(&transport(&input).unwrap().unwrap())
                        .unwrap()
                        .is_valid(&reply)
                );
                assert_eq!(strict_outputs(&outputs).unwrap(), None, "{payload}");
                assert_eq!(strict(&input).unwrap(), None, "{payload}");
            }
            // Standalone typed branches still support synthetic nulls. Scalar unions
            // have no sibling structural rewrite and remain safe to constrain natively.
            let mut standalone = json!({});
            standalone[keyword] = json!([branch_a, branch_b]);
            let mut scalar = json!({"type":["string","null"]});
            scalar[keyword] = json!([{"const":"safe"},{"type":"null"}]);
            for (payload, instance) in [
                (standalone, json!({"a":"x"})),
                (scalar.clone(), json!("safe")),
                (scalar, Value::Null),
            ] {
                assert!(
                    super::super::schema_validator(&payload)
                        .unwrap()
                        .is_valid(&instance)
                );
                let input = request(BTreeMap::from([("result".into(), payload)]));
                let projected = strict(&input).unwrap().unwrap();
                validate_strict(&projected).unwrap();
                let reply = json!({"schema":"af.worker-reply/1","outputs":{"result":[instance]}});
                assert!(
                    super::super::schema_validator(&projected)
                        .unwrap()
                        .is_valid(&reply)
                );
            }
        }
    }

    #[test]
    fn explicit_scalars_ignore_object_keywords_and_union_null_is_preserved() {
        let payload = json!({"type":"string","const":"x","properties":{"x":{"type":"string"}},"items":{"type":"string"}});
        let projected = strict(&request(BTreeMap::from([("result".into(), payload)])))
            .unwrap()
            .unwrap();
        let item = &projected["properties"]["outputs"]["properties"]["result"]["items"];
        assert_eq!(item, &json!({"type":"string","enum":["x"]}));
        let payload = json!({"anyOf":[
            {"type":"object","properties":{"a":{"type":"string"}},"additionalProperties":false},
            {"type":"object","properties":{"a":{"type":"null"}},"additionalProperties":false}
        ]});
        let input = request(BTreeMap::from([("result".into(), payload)]));
        let reply =
            br#"{"schema":"af.worker-reply/1","outputs":{"result":[{"a":null}],"unknown":[]}}"#;
        let normalized: Value =
            serde_json::from_slice(&normalize_strict_reply(&input, reply)).unwrap();
        assert_eq!(normalized["outputs"]["result"][0], json!({"a":null}));
        assert!(
            normalized["outputs"].get("unknown").is_some(),
            "normalization cannot erase undeclared ports before kernel validation"
        );
    }
}
