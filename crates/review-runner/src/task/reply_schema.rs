//! The native output schema of a Worker's reply. A typed Worker request derives it from its own
//! captured output schemas (ADR-0103, ADR-0208); a review-Campaign reviewer's prompt carries the
//! flat `ReviewerResult@2` contract, whose schema is [`reviewer_result`] (ADR-0246). Every
//! adapter that constrains a reply natively builds it here, through [`ReplyContract`], so the two
//! cannot drift: Claude Code receives [`ReplyContract::transport`] inline as `--json-schema`;
//! Codex receives [`ReplyContract::strict`] as the `--output-schema` file.
//!
//! The native facility only refuses a structurally wrong reply before it is charged as a
//! reply failure. It never replaces the kernel's validation: the captured contract, or the
//! Reviewer Result parser and admission, still admits or refuses every reply in full.

use serde_json::{Map, Value, json};

use super::{MAX_WORKER_BYTES, WORKER_REPLY_FORMAT, WorkerRequest};

/// The reply contract one native call is constrained to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyContract {
    /// The typed request's own captured output schemas (ADR-0103, ADR-0208). A captured legacy
    /// prompt has none and runs unconstrained.
    Request,
    /// The flat `ReviewerResult@2` of a review-Campaign reviewer with its proposal and notes
    /// transport fields, whatever prompt carries it (ADR-0246).
    ReviewerResult,
}

impl ReplyContract {
    /// The schema an inline facility (Claude Code `--json-schema`) receives: optional fields
    /// stay optional. None when the call runs unconstrained.
    pub fn transport(self, input: &[u8]) -> Result<Option<Value>, String> {
        match self {
            Self::Request => transport(input),
            Self::ReviewerResult => Ok(Some(reviewer_result())),
        }
    }

    /// The schema a strict facility (Codex `--output-schema`) receives: every property
    /// required, an optional one nullable. None when the call runs unconstrained.
    pub fn strict(self, input: &[u8]) -> Result<Option<Value>, String> {
        match self {
            Self::Request => strict(input),
            Self::ReviewerResult => project(&reviewer_result(), 0)
                .map(Some)
                .ok_or_else(|| "The Reviewer Result schema has no strict projection".into()),
        }
    }

    /// Remove what [`ReplyContract::strict`] made the model write and the contract never asked
    /// for. A reply that is not JSON is returned unchanged for the kernel to judge.
    pub fn normalize_strict_reply(self, input: &[u8], reply: &[u8]) -> Vec<u8> {
        match self {
            Self::Request => normalize_strict_reply(input, reply),
            Self::ReviewerResult => {
                let Ok(mut value) = serde_json::from_slice::<Value>(reply) else {
                    return reply.to_vec();
                };
                strip_optional_nulls(&mut value, &reviewer_result(), 0);
                serde_json::to_vec(&value).unwrap_or_else(|_| reply.to_vec())
            }
        }
    }
}

/// The structural schema of a review-Campaign reviewer's final message: the `ReviewerResult@2`
/// fields of `schemas/reviewer-result-v2.json` with their types, required fields, enums and
/// closed objects, beside the two transport fields a reply may carry next to the flat result,
/// the optional `proposal` (ADR-0038) and `notes` (warm layers). Every value rule the result
/// schema adds — string bounds and patterns, the canonical path grammar, the line and confidence
/// ranges, the change-wide line rule — and every rule of the Campaign itself, such as one
/// disposition per assigned prior Finding, stays the kernel's, unchanged (ADR-0246).
pub fn reviewer_result() -> Value {
    let string = || json!({"type":"string"});
    let strings = || json!({"type":"array","items":{"type":"string"}});
    json!({
        "type":"object", "additionalProperties":false,
        "required":["reports","benchmark_demands","dispositions"],
        "properties":{
            "reports":{"type":"array","items":{
                "type":"object", "additionalProperties":false,
                "required":["severity","file","title","body","fix"],
                "properties":{
                    "severity":{"enum":["blocker","major","minor"]},
                    "file":string(),
                    "line":{"type":["integer","null"]},
                    "title":string(),
                    "body":string(),
                    "fix":string(),
                    "confidence":{"type":["number","null"]},
                    "rule_id":string(),
                    "occurrence_key":string()
                }
            }},
            "benchmark_demands":{"type":"array","items":{
                "type":"object", "additionalProperties":false,
                "required":["claim","why","suggested_method"],
                "properties":{"claim":string(),"why":string(),"suggested_method":string()}
            }},
            "dispositions":{"type":"array","items":{
                "type":"object", "additionalProperties":false,
                "required":["finding_id","position","reason"],
                "properties":{
                    "finding_id":string(),
                    "position":{"enum":["corroborate","not_reproduced","dispute"]},
                    "reason":string()
                }
            }},
            "proposal":{
                "type":"object", "additionalProperties":false,
                "required":["patch","paths","description"],
                "properties":{
                    "patch":string(),
                    "report_indexes":{"type":"array","items":{"type":"integer"}},
                    "finding_ids":strings(),
                    "evidence_ids":strings(),
                    "paths":strings(),
                    "description":string(),
                    "auto_apply_nominated":{"type":"boolean"}
                }
            },
            "notes":{
                "type":"object", "additionalProperties":false,
                "properties":{
                    "inspected":strings(),
                    "model_of_change":string(),
                    "open_questions":strings(),
                    "hints":{"type":"array","items":{
                        "type":"object", "additionalProperties":false,
                        "required":["path","note"],
                        "properties":{"path":string(),"note":string()}
                    }}
                }
            }
        }
    })
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
            .any(|p| !review_core::task::is_name(p))
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
        // Captured contracts already forbid $id and external references. No resolver or schema
        // simplification is added here; the original Kernel validator remains authoritative.
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
/// reference, a schema without a type.
fn project(schema: &Value, depth: usize) -> Option<Value> {
    if depth > 32 {
        return None;
    }
    let schema = schema.as_object()?;
    if schema.contains_key("$ref") || schema.contains_key("$defs") {
        return None;
    }
    let mut projected = Map::new();
    for (key, value) in schema {
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
            "items" => project(value, depth + 1)?,
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
    let is_object = schema.get("type").and_then(Value::as_str) == Some("object")
        || schema.contains_key("properties");
    if is_object {
        // A strict object is closed: an open map has no strict form.
        if schema
            .get("additionalProperties")
            .is_some_and(|value| value != &Value::Bool(false))
            || !schema.contains_key("additionalProperties") && !schema.contains_key("properties")
        {
            return None;
        }
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut properties = Map::new();
        if let Some(declared) = schema.get("properties") {
            for (name, property) in declared.as_object()? {
                let property = project(property, depth + 1)?;
                properties.insert(
                    name.clone(),
                    if required.contains(&name.as_str()) {
                        property
                    } else {
                        json!({"anyOf":[property, {"type":"null"}]})
                    },
                );
            }
        }
        projected.insert("type".into(), json!("object"));
        projected.insert(
            "required".into(),
            Value::Array(properties.keys().cloned().map(Value::String).collect()),
        );
        projected.insert("properties".into(), Value::Object(properties));
        projected.insert("additionalProperties".into(), Value::Bool(false));
    }
    if !projected.contains_key("type")
        && !projected.contains_key("anyOf")
        && !projected.contains_key("enum")
        && !projected.contains_key("const")
    {
        return None;
    }
    Some(Value::Object(projected))
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
    let mut properties = Map::new();
    for (port, payload) in &request.output_schemas {
        let Some(payload) = project(payload, 0) else {
            return Ok(None);
        };
        properties.insert(port.clone(), json!({"type":"array","items":payload}));
    }
    Ok(Some(json!({
        "type":"object", "additionalProperties":false, "required":["schema","outputs"],
        "properties":{
            "schema":{"type":"string","enum":["af.worker-reply/1"]},
            "outputs":{
                "type":"object", "additionalProperties":false,
                "required": properties.keys().cloned().map(Value::String).collect::<Vec<_>>(),
                "properties":properties
            }
        }
    })))
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
    outputs.retain(|_, items| items.as_array().is_none_or(|items| !items.is_empty()));
    for (port, items) in outputs.iter_mut() {
        let (Some(schema), Some(items)) = (request.output_schemas.get(port), items.as_array_mut())
        else {
            continue;
        };
        for item in items {
            strip_optional_nulls(item, schema, 0);
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| reply.to_vec())
}

fn strip_optional_nulls(value: &mut Value, schema: &Value, depth: usize) {
    if depth > 32 {
        return;
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
        object.retain(|name, field| !field.is_null() || required.contains(&name.as_str()));
        for (name, field) in object.iter_mut() {
            if let Some(property) = properties.get(name) {
                strip_optional_nulls(field, property, depth + 1);
            }
        }
    }
    if let (Some(items), Some(item_schema)) = (value.as_array_mut(), schema.get("items")) {
        for item in items {
            strip_optional_nulls(item, item_schema, depth + 1);
        }
    }
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
            json!({"type":"object","properties":{"a":{"$ref":"#/$defs/a"}},"additionalProperties":false}),
            json!(true),
        ] {
            let schemas = BTreeMap::from([("result".to_string(), payload)]);
            assert_eq!(strict(&request(schemas)).unwrap(), None);
        }
        assert_eq!(strict(b"a legacy prompt").unwrap(), None);
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

    /// The structural part of a published schema: its types, required fields, enums, closed
    /// objects and items, with local references inlined. What [`reviewer_result`] must equal.
    fn structural(schema: &Value, root: &Value) -> Value {
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            let name = reference.strip_prefix("#/$defs/").unwrap();
            return structural(&root["$defs"][name], root);
        }
        let mut kept = Map::new();
        for (key, value) in schema.as_object().unwrap() {
            let value = match key.as_str() {
                "type" | "required" | "additionalProperties" | "enum" => value.clone(),
                "items" => structural(value, root),
                "properties" => Value::Object(
                    value
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(name, property)| (name.clone(), structural(property, root)))
                        .collect(),
                ),
                _ => continue,
            };
            kept.insert(key.clone(), value);
        }
        Value::Object(kept)
    }

    /// ADR-0246: the Campaign reviewer's native schema is the structure of the published
    /// `ReviewerResult@2` schema, nothing more and nothing less, beside the two transport fields
    /// whose properties are exactly the fields of their Rust declarations.
    #[test]
    fn the_reviewer_schema_is_the_published_result_structure_and_its_transport_fields() {
        let published: Value =
            serde_json::from_str(include_str!("../../../../schemas/reviewer-result-v2.json"))
                .unwrap();
        let mut native = reviewer_result();
        let properties = native["properties"].as_object_mut().unwrap();
        let proposal = properties.remove("proposal").unwrap();
        let notes = properties.remove("notes").unwrap();
        assert_eq!(native, structural(&published, &published));

        let declared = |value: Value| {
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        };
        let full_proposal = serde_json::to_value(crate::ReviewerProposalDeclaration {
            patch: "diff".into(),
            report_indexes: vec![0],
            finding_ids: vec!["F".into()],
            evidence_ids: vec!["E".into()],
            paths: vec!["a.rs".into()],
            description: "d".into(),
            auto_apply_nominated: true,
        })
        .unwrap();
        let full_notes = serde_json::to_value(crate::ReviewerNotesDeclaration {
            inspected: vec!["a.rs".into()],
            model_of_change: "m".into(),
            open_questions: vec!["q".into()],
            hints: vec![crate::ReviewerNoteHint {
                path: "a.rs".into(),
                note: "n".into(),
            }],
        })
        .unwrap();
        assert_eq!(
            declared(proposal["properties"].clone()),
            declared(full_proposal.clone())
        );
        assert_eq!(
            declared(notes["properties"].clone()),
            declared(full_notes.clone())
        );
        let validator = jsonschema::draft202012::new(&reviewer_result()).unwrap();
        let reply = json!({"reports":[],"benchmark_demands":[],"dispositions":[],
            "proposal":full_proposal,"notes":full_notes});
        assert!(validator.is_valid(&reply), "{reply}");
        // Structurally wrong replies the native facility refuses: the `findings` alias, a
        // missing field, a field the contract does not define, a severity outside the enum.
        for wrong in [
            json!({"findings":[],"benchmark_demands":[],"dispositions":[]}),
            json!({"reports":[],"benchmark_demands":[]}),
            json!({"reports":[],"benchmark_demands":[],"dispositions":[],"summary":"ok"}),
            json!({"reports":[{"severity":"critical","file":"","title":"t","body":"b","fix":"f"}],
                "benchmark_demands":[],"dispositions":[]}),
        ] {
            assert!(!validator.is_valid(&wrong), "{wrong}");
        }
    }

    /// ADR-0246: Codex receives the strict projection of the reviewer schema, every property
    /// required and every optional one nullable, and what it made the model write — `null` for
    /// an absent proposal, notes, line or rule — is removed before the kernel parses the reply.
    #[test]
    fn the_reviewer_strict_projection_round_trips_to_the_flat_result() {
        let strict = ReplyContract::ReviewerResult.strict(b"").unwrap().unwrap();
        let required: std::collections::BTreeSet<_> = strict["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        assert_eq!(
            required,
            std::collections::BTreeSet::from([
                "benchmark_demands",
                "dispositions",
                "notes",
                "proposal",
                "reports"
            ])
        );
        assert_eq!(
            strict["properties"]["proposal"]["anyOf"][1],
            json!({"type":"null"})
        );
        let report = &strict["properties"]["reports"]["items"];
        assert_eq!(report["additionalProperties"], json!(false));
        assert_eq!(
            report["required"].as_array().unwrap().len(),
            report["properties"].as_object().unwrap().len()
        );
        let written = br#"{"reports":[{"severity":"minor","file":"a.rs","line":null,"title":"t","body":"b","fix":"f","confidence":0.5,"rule_id":null,"occurrence_key":null}],"benchmark_demands":[],"dispositions":[],"proposal":null,"notes":null}"#;
        let validator = jsonschema::draft202012::new(&strict).unwrap();
        assert!(validator.is_valid(&serde_json::from_slice(written).unwrap()));
        let normalized = ReplyContract::ReviewerResult.normalize_strict_reply(b"", written);
        assert_eq!(
            serde_json::from_slice::<Value>(&normalized).unwrap(),
            json!({"reports":[{"severity":"minor","file":"a.rs","title":"t","body":"b","fix":"f","confidence":0.5}],
                "benchmark_demands":[],"dispositions":[]})
        );
        let parsed = crate::parse_reviewer_result(std::str::from_utf8(&normalized).unwrap());
        assert_eq!(parsed.unwrap().reports.len(), 1);
        assert_eq!(
            crate::parse_proposal_declaration(std::str::from_utf8(&normalized).unwrap()),
            Ok(None)
        );
        assert_eq!(
            ReplyContract::ReviewerResult.normalize_strict_reply(b"", b"not json"),
            b"not json"
        );
        // The typed-request contract is the one it always was.
        assert_eq!(ReplyContract::Request.strict(b"a legacy prompt"), Ok(None));
        assert_eq!(
            ReplyContract::ReviewerResult.transport(b"a legacy prompt"),
            Ok(Some(reviewer_result()))
        );
    }
}
