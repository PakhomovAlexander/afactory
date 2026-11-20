//! Pure bounded validation of the emitted strict dialect. It never resolves references or
//! traverses const/enum values as schemas. Original payload validation remains authoritative.
use serde_json::Value;

pub(super) fn pointer(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}
pub(super) fn scalar_type(value: &Value) -> Option<&'static str> {
    Some(match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::String(_) => "string",
        Value::Number(_) => "number",
        _ => return None,
    })
}
pub(super) fn has_type(types: Option<&Value>, kind: &str) -> bool {
    types.is_some_and(|types| {
        types.as_str() == Some(kind)
            || types
                .as_array()
                .is_some_and(|types| types.iter().any(|t| t.as_str() == Some(kind)))
    })
}
pub(super) fn allows_type(types: &Value, value: &Value) -> bool {
    let kind = match value {
        Value::Object(_) => "object",
        Value::Array(_) => "array",
        _ => scalar_type(value).expect("scalar"),
    };
    has_type(Some(types), kind)
        || kind == "number"
            && has_type(Some(types), "integer")
            && value.as_f64().is_some_and(|v| v.fract() == 0.0)
}

fn bounded(path: &str, depth: usize, nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    if depth > 32 || *nodes > 16384 {
        return Err(format!("Strict schema {path}: exceeds depth/node bound"));
    }
    Ok(())
}

/// Impossible literal restrictions are valid kernel schemas with no strict projection.
/// Reuse kernel numeric equality, including nested literal values.
pub(super) fn compatible_literals(schema: &Value) -> bool {
    let Some(values) = schema.get("const").map(std::slice::from_ref).or_else(|| {
        schema
            .get("enum")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
    }) else {
        return true;
    };
    let restrictions: serde_json::Map<String, Value> = ["type", "const", "enum"]
        .into_iter()
        .filter_map(|key| schema.get(key).map(|v| (key.into(), v.clone())))
        .collect();
    super::super::schema_validator(&Value::Object(restrictions))
        .is_ok_and(|validator| values.iter().any(|value| validator.is_valid(value)))
}

/// Validate the actual emitted strict schema, with actionable JSON Pointer diagnostics.
/// This is a structural provider dialect check, not a substitute for kernel semantics.
pub fn validate_strict(schema: &Value) -> Result<(), String> {
    fn visit(schema: &Value, path: &str, depth: usize, nodes: &mut usize) -> Result<(), String> {
        bounded(path, depth, nodes)?;
        let fail = |reason| format!("Strict schema {path}: {reason}");
        let map = schema
            .as_object()
            .ok_or_else(|| fail("expected schema object"))?;
        if map.len() > 8 {
            return Err(fail("too many emitted keywords"));
        }
        for key in map.keys() {
            if ![
                "type",
                "enum",
                "const",
                "anyOf",
                "properties",
                "required",
                "additionalProperties",
                "items",
            ]
            .contains(&key.as_str())
            {
                return Err(fail("unsupported emitted keyword"));
            }
        }
        let types: Vec<_> = match map.get("type") {
            Some(Value::String(t)) => vec![t.as_str()],
            Some(Value::Array(ts)) if !ts.is_empty() && ts.len() <= 7 => ts
                .iter()
                .map(|t| t.as_str().ok_or_else(|| fail("invalid type")))
                .collect::<Result<_, _>>()?,
            None if map.contains_key("anyOf") => vec![],
            _ => return Err(fail("schema must specify a type or anyOf")),
        };
        for (i, t) in types.iter().enumerate() {
            if ![
                "object", "array", "string", "number", "integer", "boolean", "null",
            ]
            .contains(t)
                || types[..i].contains(t)
            {
                return Err(fail("invalid or duplicate type"));
            }
        }
        if let Some(branches) = map.get("anyOf") {
            let branches = branches
                .as_array()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| fail("anyOf must be nonempty"))?;
            for (i, branch) in branches.iter().enumerate() {
                visit(branch, &format!("{path}/anyOf/{i}"), depth + 1, nodes)?;
            }
        }
        if let Some(values) = map.get("enum") {
            let values = values
                .as_array()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| fail("enum must be nonempty"))?;
            *nodes += values.len();
            if *nodes > 16384 {
                return Err(fail("enum exceeds node bound"));
            }
            if values.iter().any(|v| {
                scalar_type(v).is_none() || map.get("type").is_none_or(|t| !allows_type(t, v))
            }) {
                return Err(fail("enum needs compatible scalar types"));
            }
        }
        if let Some(value) = map.get("const")
            && (scalar_type(value).is_none()
                || map.get("type").is_none_or(|t| !allows_type(t, value)))
        {
            return Err(fail("const needs compatible scalar type"));
        }
        if types.contains(&"object") {
            let properties = map
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| fail("object needs properties"))?;
            let required = map
                .get("required")
                .and_then(Value::as_array)
                .ok_or_else(|| fail("object needs required"))?;
            if properties.len() > 16384 || required.len() > 16384 {
                return Err(fail("object exceeds node bound"));
            }
            let names: std::collections::BTreeSet<_> =
                required.iter().filter_map(Value::as_str).collect();
            if map.get("additionalProperties") != Some(&Value::Bool(false))
                || names.len() != required.len()
                || names.len() != properties.len()
                || properties.keys().any(|n| !names.contains(n.as_str()))
            {
                return Err(fail(
                    "object must be closed and require exactly every property",
                ));
            }
            for (name, child) in properties {
                visit(
                    child,
                    &format!("{path}/properties/{}", pointer(name)),
                    depth + 1,
                    nodes,
                )?;
            }
        } else if ["properties", "required", "additionalProperties"]
            .iter()
            .any(|k| map.contains_key(*k))
        {
            return Err(fail("object keywords need object type"));
        }
        if types.contains(&"array") {
            visit(
                map.get("items").ok_or_else(|| fail("array needs items"))?,
                &format!("{path}/items"),
                depth + 1,
                nodes,
            )?;
        } else if map.contains_key("items") {
            return Err(fail("items needs array type"));
        }
        Ok(())
    }
    visit(schema, "", 0, &mut 0)
}
