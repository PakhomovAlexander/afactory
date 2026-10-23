//! Why a Task file does not parse, by the field path and the shape that field takes.
//!
//! A Task file that does not deserialize is read again as plain JSON (a TOML file converted to
//! it) and walked against the shape `af.task-file/1` declares (`schemas/task-file-v1.json`),
//! so the refusal names the field a person wrote, the value they wrote and the value it takes:
//! `task file ticket.json: limits.tokens must be an integer from 0 to 9007199254740991, got
//! "6000000" (string)`. Nested shapes owned elsewhere are judged by their own types, under
//! their field path. Only when the walk finds nothing is the parser's own text shown, still
//! under the file's name.

use std::path::Path;

use serde_json::Value;

const SAFE: u64 = review_core::json::SAFE_INTEGER_MAX as u64;

/// Every field of `af.task-file/1`, in the order the schema declares them.
const FIELDS: [&str; 15] = [
    "schema",
    "task_id",
    "kind",
    "goal",
    "requirements",
    "inputs",
    "document_sources",
    "report_sources",
    "optimization_history",
    "issue",
    "pipeline",
    "strategy",
    "verification",
    "facts",
    "limits",
];

const REQUIRED: [&str; 6] = ["schema", "task_id", "kind", "goal", "strategy", "limits"];

const LIMITS: [&str; 4] = ["tokens", "max_attempts", "wall_ms", "verification"];

/// A value as a refusal quotes it: compact, at most 40 characters, then its JSON type.
fn quoted(value: &Value) -> String {
    let kind = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "table",
    };
    let mut text = value.to_string();
    if text.chars().count() > 40 {
        text = text.chars().take(39).collect::<String>() + "…";
    }
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    format!("{text} ({kind})")
}

fn text(path: &str, value: &Value) -> Option<String> {
    (!value.is_string()).then(|| format!("{path} must be a string, got {}", quoted(value)))
}

fn integer(path: &str, value: &Value, min: u64, max: u64) -> Option<String> {
    let fits = value.as_u64().is_some_and(|n| (min..=max).contains(&n));
    (!fits).then(|| {
        format!(
            "{path} must be an integer from {min} to {max}, got {}",
            quoted(value)
        )
    })
}

fn table(path: &str, value: &Value) -> Option<String> {
    (!value.is_object()).then(|| format!("{path} must be a table, got {}", quoted(value)))
}

/// `value` judged by the type that owns its shape, the parser's words under `path`.
fn typed<T: serde::de::DeserializeOwned>(path: &str, value: &Value) -> Option<String> {
    serde_json::from_value::<T>(value.clone())
        .err()
        .map(|error| format!("{path}: {error}"))
}

/// The first field of `document` that does not have the shape `af.task-file/1` takes, said as
/// its field path, what it takes and what it holds; `None` when every field has its shape.
pub(super) fn diagnose(document: &Value) -> Option<String> {
    let Some(fields) = document.as_object() else {
        return Some(format!(
            "a Task file is a table of fields, got {}",
            quoted(document)
        ));
    };
    if let Some(unknown) = fields.keys().find(|key| !FIELDS.contains(&key.as_str())) {
        return Some(format!(
            "{unknown} is not a Task file field; its fields are {}",
            FIELDS.join(", ")
        ));
    }
    if let Some(missing) = REQUIRED.iter().find(|field| !fields.contains_key(**field)) {
        return Some(format!(
            "{missing} is missing; a Task file requires {}",
            REQUIRED.join(", ")
        ));
    }
    for (field, value) in fields {
        let path = field.as_str();
        let wrong = match path {
            "schema"
            | "task_id"
            | "kind"
            | "goal"
            | "strategy"
            | "document_sources"
            | "report_sources"
            | "optimization_history" => text(path, value),
            "requirements" => table(path, value),
            "inputs" => table(path, value).or_else(|| {
                value.as_object().and_then(|ports| {
                    ports.iter().find_map(|(port, binding)| {
                        typed::<super::input_bindings::TaskInputRefV1>(
                            &format!("inputs.{port}"),
                            binding,
                        )
                    })
                })
            }),
            "issue" => typed::<super::issue::IssueSource>(path, value),
            "pipeline" => typed::<review_core::task::PipelineChoiceV1>(path, value),
            "verification" => {
                let known = ["evaluation", "review", "review_or_targeted_fixes"];
                (!value.as_str().is_some_and(|word| known.contains(&word))).then(|| {
                    format!(
                        "verification must be one of {}, got {}",
                        known.join(", "),
                        quoted(value)
                    )
                })
            }
            "facts" => table(path, value).or_else(|| {
                value.as_object().and_then(|facts| {
                    facts.iter().find_map(|(name, fact)| {
                        let simple =
                            fact.is_boolean() || fact.is_string() || fact.as_i64().is_some();
                        (!simple).then(|| {
                            format!(
                                "facts.{name} must be a boolean, a string or an integer, got {}",
                                quoted(fact)
                            )
                        })
                    })
                })
            }),
            "limits" => limits(value),
            _ => None,
        };
        if wrong.is_some() {
            return wrong;
        }
    }
    None
}

fn limits(value: &Value) -> Option<String> {
    let Some(limits) = value.as_object() else {
        return table("limits", value);
    };
    if let Some(unknown) = limits.keys().find(|key| !LIMITS.contains(&key.as_str())) {
        return Some(format!(
            "limits.{unknown} is not a limit; the limits are {}",
            LIMITS.join(", ")
        ));
    }
    if let Some(missing) = LIMITS.iter().find(|field| !limits.contains_key(**field)) {
        return Some(format!(
            "limits.{missing} is missing; limits requires {}",
            LIMITS.join(", ")
        ));
    }
    integer("limits.tokens", &limits["tokens"], 0, SAFE)
        .or_else(|| {
            integer(
                "limits.max_attempts",
                &limits["max_attempts"],
                1,
                u64::from(u32::MAX),
            )
        })
        .or_else(|| integer("limits.wall_ms", &limits["wall_ms"], 0, u64::MAX))
        .or_else(|| {
            let reserve = &limits["verification"];
            let Some(fields) = reserve.as_object() else {
                return table("limits.verification", reserve);
            };
            let known = ["tokens", "attempts", "wall_ms"];
            if let Some(unknown) = fields.keys().find(|key| !known.contains(&key.as_str())) {
                return Some(format!(
                    "limits.verification.{unknown} is not part of the reserve; it holds {}",
                    known.join(", ")
                ));
            }
            known
                .iter()
                .find(|field| !fields.contains_key(**field))
                .map(|missing| {
                    format!(
                        "limits.verification.{missing} is missing; the reserve holds {}",
                        known.join(", ")
                    )
                })
                .or_else(|| integer("limits.verification.tokens", &fields["tokens"], 0, SAFE))
                .or_else(|| {
                    integer(
                        "limits.verification.attempts",
                        &fields["attempts"],
                        0,
                        u64::from(u32::MAX),
                    )
                })
                .or_else(|| {
                    integer(
                        "limits.verification.wall_ms",
                        &fields["wall_ms"],
                        0,
                        u64::MAX,
                    )
                })
        })
}

/// The document a Task file holds, as JSON: a `.json` file as written, any other one read as
/// TOML. `Err` is the parser's own complaint about the syntax.
fn document(path: &Path, bytes: &[u8]) -> Result<Value, String> {
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        serde_json::from_slice(bytes).map_err(|error| format!("is not valid JSON: {error}"))
    } else {
        let text =
            std::str::from_utf8(bytes).map_err(|error| format!("is not UTF-8 text: {error}"))?;
        let value: toml::Value =
            toml::from_str(text).map_err(|error| format!("is not valid TOML: {error}"))?;
        serde_json::to_value(value).map_err(|error| error.to_string())
    }
}

/// The refusal of a Task file `parser` could not read: the field path and the shape it takes
/// when the walk finds one, else the parser's own text; always under the file's name.
pub(super) fn refusal(path: &Path, bytes: &[u8], parser: &str) -> String {
    let named = format!("task file {}", path.display());
    match document(path, bytes) {
        Err(syntax) => format!("{named} {syntax}"),
        Ok(document) => match diagnose(&document) {
            Some(why) => format!("{named}: {why}"),
            None => format!("{named}: {parser}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid() -> Value {
        json!({
            "schema": "af.task-file/1",
            "task_id": "t-1",
            "kind": "software",
            "goal": "do it",
            "strategy": "light",
            "limits": {
                "tokens": 6000000,
                "max_attempts": 3,
                "wall_ms": 60000,
                "verification": {"tokens": 1000, "attempts": 1, "wall_ms": 1000}
            }
        })
    }

    #[test]
    fn a_valid_shape_has_nothing_to_say() {
        assert_eq!(diagnose(&valid()), None);
    }

    #[test]
    fn each_wrong_field_is_named_by_its_path_and_shape() {
        let mut tokens = valid();
        tokens["limits"]["tokens"] = json!("6000000");
        assert_eq!(
            diagnose(&tokens).unwrap(),
            "limits.tokens must be an integer from 0 to 9007199254740991, got \"6000000\" (string)"
        );
        let mut unknown = valid();
        unknown["goals"] = json!("x");
        assert!(
            diagnose(&unknown)
                .unwrap()
                .starts_with("goals is not a Task file field; its fields are schema, task_id")
        );
        let mut missing = valid();
        missing.as_object_mut().unwrap().remove("goal");
        assert!(diagnose(&missing).unwrap().starts_with("goal is missing"));
        let mut reserve = valid();
        reserve["limits"]["verification"]["attempts"] = json!(-1);
        assert_eq!(
            diagnose(&reserve).unwrap(),
            "limits.verification.attempts must be an integer from 0 to 4294967295, got -1 (number)"
        );
        let mut verification = valid();
        verification["verification"] = json!("strict");
        assert!(
            diagnose(&verification)
                .unwrap()
                .starts_with("verification must be one of evaluation, review")
        );
        let mut fact = valid();
        fact["facts"] = json!({"size": [1, 2]});
        assert!(diagnose(&fact).unwrap().starts_with("facts.size must be"));
    }

    #[test]
    fn toml_files_are_read_as_toml_and_syntax_keeps_the_parsers_words() {
        let path = Path::new("ticket.toml");
        let refused = refusal(
            path,
            b"schema = \"af.task-file/1\"\ntask_id = 7\nkind = \"software\"\ngoal = \"g\"\n\
              strategy = \"light\"\n[limits]\ntokens = 1\nmax_attempts = 1\nwall_ms = 1\n\
              [limits.verification]\ntokens = 0\nattempts = 0\nwall_ms = 0\n",
            "invalid type",
        );
        assert_eq!(
            refused,
            "task file ticket.toml: task_id must be a string, got 7 (number)"
        );
        let broken = refusal(Path::new("ticket.json"), b"{", "eof");
        assert!(
            broken.starts_with("task file ticket.json is not valid JSON: "),
            "{broken}"
        );
    }
}
