//! Native structured transport is prospective: never salvage a textual or stored failed reply.
use review_runner::task::MAX_WORKER_BYTES;
use review_runner::task::reply_schema::ReplyContract;
use serde_json::Value;

// The native CLI accepts an inline schema, not a file. Leave room for the process environment
// and fixed arguments below the supported hosts' argv ceiling; the prompt still uses stdin.
const MAX_SCHEMA_ARGUMENT_BYTES: usize = 64 * 1024;

pub(super) fn output_schema(
    contract: ReplyContract,
    input: &[u8],
) -> Result<Option<String>, String> {
    // The one derivation of a reply's native schema (ADR-0103, ADR-0246), shared with Codex.
    let Some(schema) = contract
        .transport(input)
        .map_err(|error| format!("Claude Worker request refused: {error}"))?
    else {
        return Ok(None);
    };
    let schema = schema.to_string();
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
