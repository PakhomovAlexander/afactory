//! Native structured transport is prospective: never salvage a textual or stored failed reply.
use review_runner::task::{MAX_WORKER_BYTES, WORKER_REPLY_FORMAT, WorkerRequest};
use serde_json::{Value, json};

// The native CLI accepts an inline schema, not a file. Leave room for the process environment
// and fixed arguments below the supported hosts' argv ceiling; the prompt still uses stdin.
const MAX_SCHEMA_ARGUMENT_BYTES: usize = 64 * 1024;

pub(super) fn output_schema(input: &[u8]) -> Result<Option<String>, String> {
    if input.len() > MAX_WORKER_BYTES {
        return Err("Claude Worker input exceeds byte bound".into());
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
    let request: WorkerRequest = serde_json::from_value(value)
        .map_err(|e| format!("Invalid Claude typed Worker request: {e}"))?;
    if request.schema != "af.worker-request/1"
        || request.reply_format != WORKER_REPLY_FORMAT
        || request.output_schemas.is_empty()
        || request.output_schemas.len() > 32
        || request
            .output_schemas
            .keys()
            .any(|p| !review_core::task::is_name(p))
    {
        return Err("Unsupported Claude typed Worker request contract".into());
    }
    let mut properties = serde_json::Map::new();
    for (index, (port, mut payload)) in request.output_schemas.into_iter().enumerate() {
        // Each captured payload schema has its own local-reference root. A transport-only
        // resource ID preserves that root when the unchanged schema is nested in the reply.
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
            _ => return Err("Invalid Claude output payload schema".into()),
        }
        properties.insert(
            port,
            json!({"type":"array","maxItems":1024,"items":payload}),
        );
    }
    let schema = json!({
        "type":"object", "additionalProperties":false, "required":["schema","outputs"],
        "properties":{
            "schema":{"type":"string","const":"af.worker-reply/1"},
            "outputs":{"type":"object","additionalProperties":false,"properties":properties}
        }
    })
    .to_string();
    if schema.len() > MAX_SCHEMA_ARGUMENT_BYTES {
        return Err("Claude structured output schema exceeds inline argument bound".into());
    }
    Ok(Some(schema))
}

pub(super) fn message(envelope: &Value, typed: bool) -> Result<Vec<u8>, String> {
    let bytes = if typed {
        // The native result text can be empty, explanatory prose, or fenced JSON. It is not
        // the schema-constrained result and cannot substitute for a missing structured_output.
        let output = envelope
            .get("structured_output")
            .filter(|v| v.is_object())
            .ok_or("Claude Worker returned no structured output object")?;
        serde_json::to_vec(output).map_err(|e| e.to_string())?
    } else {
        envelope
            .get("result")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or("Claude Worker returned no final message")?
            .as_bytes()
            .to_vec()
    };
    if bytes.len() > MAX_WORKER_BYTES {
        return Err("Claude Worker reply exceeds byte bound".into());
    }
    Ok(bytes)
}
