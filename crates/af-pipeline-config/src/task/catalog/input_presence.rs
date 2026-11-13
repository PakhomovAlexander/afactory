//! Bounded, conservative proof of root Worker input presence, not payload satisfiability.
//! Future artifact values stay symbolic. Inclusive alternatives need a known accepting branch
//! or safe presence on every branch; multiple exclusive alternatives require proven branch truth.
use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Clone, Copy)]
struct Proof {
    truth: Option<bool>,
    safe: bool,
}

fn conjunction(values: &[Proof]) -> Proof {
    Proof {
        truth: if values.iter().any(|p| p.truth == Some(false)) {
            Some(false)
        } else if values.iter().all(|p| p.truth == Some(true)) {
            Some(true)
        } else {
            None
        },
        safe: values.iter().all(|p| p.safe),
    }
}

pub(super) fn validate(
    schema: &Value,
    supply: &BTreeMap<String, Option<bool>>,
) -> Result<(), String> {
    // Apply the exact runtime schema policy, including local-only reference resolution.
    af_runner::task::schema_validator(schema)?;
    let proof = evaluate(schema, schema, supply, false, 0, &mut 0)?;
    if proof.safe {
        Ok(())
    } else {
        Err(
            "Worker input presence cannot be proved from compiled wiring and bound Task inputs"
                .into(),
        )
    }
}

fn evaluate(
    schema: &Value,
    root: &Value,
    supply: &BTreeMap<String, Option<bool>>,
    symbolic: bool,
    depth: usize,
    visits: &mut usize,
) -> Result<Proof, String> {
    *visits += 1;
    if depth > 64 || *visits > 16384 {
        return Err("Worker input presence proof exceeds local resolution bounds".into());
    }
    let exact = |value| Proof {
        truth: Some(value),
        safe: value,
    };
    if let Some(value) = schema.as_bool() {
        return Ok(exact(value));
    }
    let map = schema.as_object().ok_or("Invalid Worker input schema")?;
    let mut terms = Vec::new();
    for (keyword, value) in map {
        let mut child = |value: &Value| evaluate(value, root, supply, symbolic, depth + 1, visits);
        // Inside a supplied property's schema, ordinary envelope/payload predicates stay
        // symbolic. Boolean applicators and references still matter: `not: {}` forbids
        // the property regardless of its future value. Never confuse nested `required`
        // names with root inputs.
        if symbolic
            && !matches!(
                keyword.as_str(),
                "$ref"
                    | "allOf"
                    | "anyOf"
                    | "oneOf"
                    | "not"
                    | "if"
                    | "then"
                    | "else"
                    | "$defs"
                    | "definitions"
                    | "title"
                    | "description"
                    | "$comment"
                    | "default"
                    | "examples"
            )
        {
            terms.push(Proof {
                truth: None,
                safe: true,
            });
            continue;
        }
        terms.push(match keyword.as_str() {
            "$ref" => {
                let reference = value.as_str().ok_or("Invalid local schema reference")?;
                let target = root
                    .pointer(
                        reference
                            .strip_prefix('#')
                            .ok_or("Nonlocal schema reference")?,
                    )
                    .ok_or("Missing local schema reference")?;
                child(target)?
            }
            "required" => {
                let names = value.as_array().ok_or("Invalid required input names")?;
                let proofs = names
                    .iter()
                    .map(|name| {
                        let present = supply
                            .get(name.as_str().unwrap_or_default())
                            .copied()
                            .unwrap_or(Some(false));
                        Proof {
                            truth: present,
                            safe: present == Some(true),
                        }
                    })
                    .collect::<Vec<_>>();
                conjunction(&proofs)
            }
            "allOf" | "anyOf" | "oneOf" => {
                let proofs = value
                    .as_array()
                    .ok_or("Invalid schema alternatives")?
                    .iter()
                    .map(&mut child)
                    .collect::<Result<Vec<_>, _>>()?;
                if keyword == "allOf" || (keyword == "oneOf" && proofs.len() == 1) {
                    // A single exclusive branch is just its schema: payload truth may
                    // remain symbolic without introducing any risk of branch overlap.
                    conjunction(&proofs)
                } else if keyword == "anyOf" {
                    let known = proofs.iter().any(|p| p.truth == Some(true));
                    Proof {
                        truth: if known {
                            Some(true)
                        } else if proofs.iter().all(|p| p.truth == Some(false)) {
                            Some(false)
                        } else {
                            None
                        },
                        safe: known || proofs.iter().all(|p| p.safe),
                    }
                } else {
                    let known = proofs.iter().all(|p| p.truth.is_some());
                    let valid =
                        known && proofs.iter().filter(|p| p.truth == Some(true)).count() == 1;
                    Proof {
                        truth: known.then_some(valid),
                        // Individual branch safety does not prove exclusive acceptance.
                        safe: valid,
                    }
                }
            }
            "not" => {
                let proof = child(value)?;
                Proof {
                    truth: proof.truth.map(|v| !v),
                    safe: proof.truth == Some(false),
                }
            }
            "if" => {
                let condition = child(value)?;
                let yes = map
                    .get("then")
                    .map(&mut child)
                    .transpose()?
                    .unwrap_or(exact(true));
                let no = map
                    .get("else")
                    .map(&mut child)
                    .transpose()?
                    .unwrap_or(exact(true));
                match condition.truth {
                    Some(true) => yes,
                    Some(false) => no,
                    None => Proof {
                        truth: if yes.truth == no.truth {
                            yes.truth
                        } else {
                            None
                        },
                        safe: yes.safe && no.safe,
                    },
                }
            }
            "dependentRequired" | "dependentSchemas" => {
                let mut proofs = Vec::new();
                for (name, dependency) in value.as_object().ok_or("Invalid input dependency")? {
                    let present = supply.get(name).copied().unwrap_or(Some(false));
                    if present == Some(false) {
                        continue;
                    }
                    let proof = if keyword == "dependentRequired" {
                        child(&serde_json::json!({"required": dependency}))?
                    } else {
                        child(dependency)?
                    };
                    proofs.push(if present == Some(true) {
                        proof
                    } else {
                        Proof {
                            truth: None,
                            safe: proof.safe,
                        }
                    });
                }
                conjunction(&proofs)
            }
            "minProperties" | "maxProperties" => {
                let bound = value.as_u64().ok_or("Invalid input property count")?;
                let minimum = supply.values().filter(|v| **v == Some(true)).count() as u64;
                let maximum = supply.values().filter(|v| **v != Some(false)).count() as u64;
                let (always, never) = if keyword == "minProperties" {
                    (minimum >= bound, maximum < bound)
                } else {
                    (maximum <= bound, minimum > bound)
                };
                Proof {
                    truth: if always {
                        Some(true)
                    } else if never {
                        Some(false)
                    } else {
                        None
                    },
                    safe: always,
                }
            }
            "additionalProperties" | "properties" | "patternProperties" => {
                // Ask the runtime validator which names this keyword applies to, using
                // Boolean schemas only. Then inspect the actual value schema separately.
                let mut selector = serde_json::json!({});
                if keyword == "additionalProperties" {
                    selector["additionalProperties"] = Value::Bool(false);
                    for name in ["properties", "patternProperties"] {
                        if let Some(properties) = map.get(name).and_then(Value::as_object) {
                            selector[name] = Value::Object(
                                properties
                                    .keys()
                                    .map(|k| (k.clone(), Value::Bool(true)))
                                    .collect(),
                            );
                        }
                    }
                }
                let constraints = if keyword == "additionalProperties" {
                    vec![(selector, value)]
                } else {
                    value
                        .as_object()
                        .ok_or("Invalid input properties")?
                        .iter()
                        .map(|(name, schema)| (serde_json::json!({keyword: {name: false}}), schema))
                        .collect()
                };
                let mut proofs = Vec::new();
                for (selector, schema) in constraints {
                    let validator = af_runner::task::schema_validator(&selector)?;
                    for (name, present) in supply {
                        if *present == Some(false)
                            || validator.is_valid(&serde_json::json!({name: null}))
                        {
                            continue;
                        }
                        let proof = evaluate(schema, root, supply, true, depth + 1, visits)?;
                        proofs.push(Proof {
                            truth: if *present == Some(true) || proof.truth == Some(true) {
                                proof.truth
                            } else {
                                None
                            },
                            safe: proof.safe,
                        });
                    }
                }
                conjunction(&proofs)
            }
            "propertyNames" => {
                // Keep every local reference rooted in the original document, including
                // references outside $defs and references reached through another reference.
                let mut names = serde_json::json!({"$defs":{"root":root},"allOf":[value]});
                rebase_local_references(&mut names);
                let validator = af_runner::task::schema_validator(&names)?;
                let allowed = supply.iter().all(|(name, present)| {
                    *present == Some(false) || validator.is_valid(&Value::String(name.clone()))
                });
                Proof {
                    truth: allowed.then_some(true),
                    safe: allowed,
                }
            }
            "unevaluatedProperties" if value != true => Proof {
                truth: None,
                safe: false,
            },
            "const" | "enum" => Proof {
                truth: None,
                safe: false,
            },
            "type" => exact(
                value == "object"
                    || value
                        .as_array()
                        .is_some_and(|types| types.iter().any(|t| t == "object")),
            ),
            "$defs" | "definitions" | "title" | "description" | "$comment" | "default"
            | "examples" | "then" | "else" => exact(true),
            // Ordinary value validation stays with WorkerContract::prepare. Such predicates
            // cannot establish a branch condition at planning time.
            "unevaluatedProperties"
            | "minItems"
            | "maxItems"
            | "uniqueItems"
            | "items"
            | "prefixItems"
            | "contains"
            | "minContains"
            | "maxContains"
            | "unevaluatedItems"
            | "minimum"
            | "maximum"
            | "exclusiveMinimum"
            | "exclusiveMaximum"
            | "multipleOf"
            | "minLength"
            | "maxLength"
            | "pattern"
            | "format"
            | "contentEncoding"
            | "contentMediaType"
            | "contentSchema" => Proof {
                truth: None,
                safe: true,
            },
            // Unknown root-presence semantics are not a successful proof.
            _ => Proof {
                truth: None,
                safe: false,
            },
        });
    }
    Ok(conjunction(&terms))
}

// The original schema has already passed the bounded, local-only schema policy.
fn rebase_local_references(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if key == "$ref" {
                    if let Some(reference) = value.as_str().and_then(|s| s.strip_prefix('#')) {
                        *value = Value::String(format!("#/$defs/root{reference}"));
                    }
                } else {
                    rebase_local_references(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(rebase_local_references),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn property_names_resolve_against_the_complete_original_root() {
        let bound = BTreeMap::from([("source".into(), Some(true))]);
        let optional = BTreeMap::from([("source".into(), None)]);
        for schema in [
            json!({"$defs":{"allowed":{"enum":["source"]}},"propertyNames":{"$ref":"#/$defs/allowed"}}),
            json!({"definitions":{"allowed":{"$ref":"#/properties/allowed"}},"properties":{"allowed":{"enum":["source"]}},"allOf":[{"propertyNames":{"$ref":"#/definitions/allowed"}}]}),
            json!({"$defs":{"a/b":{"enum":["source"]}},"propertyNames":{"$ref":"#/$defs/a~1b"}}),
        ] {
            let runtime = af_runner::task::schema_validator(&schema).unwrap();
            assert!(runtime.is_valid(&json!({"source":[]})));
            assert!(!runtime.is_valid(&json!({"other":[]})));
            validate(&schema, &bound).unwrap();
            validate(&schema, &optional).unwrap();
            validate(&schema, &BTreeMap::new()).unwrap();
            for presence in [Some(true), None] {
                assert!(validate(&schema, &BTreeMap::from([("other".into(), presence)])).is_err());
            }
        }
    }

    #[test]
    fn selected_inputs_are_checked_after_addition_and_raw_knowledge_replacement() {
        use af_contracts::task::{knowledge::replaced_input_names, worker_inputs::project};
        let ordinary = BTreeMap::from([
            ("source".into(), Some(true)),
            ("knowledge_query".into(), Some(true)),
            ("notes".into(), None),
        ]);
        let replaced = replaced_input_names([
            ("source", "af/SourceTree@1"),
            ("knowledge_query", "af/KnowledgeQuery@1"),
            ("notes", "af/WorkerNotes@1"),
        ]);
        for warm in [false, true] {
            let supply = project(
                ordinary.clone(),
                warm.then_some(Some(true)),
                Some(Some(true)),
                &replaced,
            );
            validate(&json!({"required":["source","knowledge"]}), &supply).unwrap();
            validate(
                &json!({"properties":{"knowledge_query":false,"notes":false}}),
                &supply,
            )
            .unwrap();
            assert!(validate(&json!({"required":["knowledge_query"]}), &supply).is_err());
            assert!(validate(&json!({"minProperties":4}), &supply).is_err());
            assert!(validate(&json!({"maxProperties":1}), &supply).is_err());
            assert_eq!(
                validate(&json!({"maxProperties":2}), &supply).is_ok(),
                !warm
            );
            assert_eq!(
                validate(&json!({"required":["development_warm"]}), &supply).is_ok(),
                warm
            );
            for schema in [
                json!({"properties":{"knowledge":false}}),
                json!({"patternProperties":{"^knowledge$":false}}),
                json!({"properties":{"source":{},"development_warm":{}},"additionalProperties":false}),
                json!({"if":{"required":["knowledge"]},"then":{"required":["knowledge_query"]}}),
                json!({"dependentRequired":{"knowledge":["knowledge_query"]}}),
                json!({"dependentSchemas":{"knowledge":{"minProperties":4}}}),
            ] {
                assert!(validate(&schema, &supply).is_err(), "{schema}");
            }
        }
        let cold = project(ordinary, None, None, &std::collections::BTreeSet::new());
        assert!(validate(&json!({"required":["knowledge"]}), &cold).is_err());
        assert!(validate(&json!({"required":["development_warm"]}), &cold).is_err());
        validate(&json!({"required":["knowledge_query"]}), &cold).unwrap();
    }

    #[test]
    fn schema_valued_property_restrictions_are_not_assumed_safe() {
        let bound = BTreeMap::from([("source".into(), Some(true))]);
        let optional = BTreeMap::from([("source".into(), None)]);
        for forbidden in [
            json!({"not":{}}),
            json!({"allOf":[{"not":{}}]}),
            json!({"anyOf":[false,{"not":{}}]}),
            json!({"$ref":"#/$defs/forbidden"}),
            json!({"if":{},"then":false}),
            json!({"oneOf":[{},{}]}),
        ] {
            for restriction in [
                json!({"additionalProperties":forbidden}),
                json!({"properties":{"source":forbidden}}),
                json!({"patternProperties":{"^source$":forbidden}}),
            ] {
                let schema = json!({"$defs":{"forbidden":{"not":{}}},"allOf":[restriction]});
                let runtime = af_runner::task::schema_validator(&schema).unwrap();
                assert!(!runtime.is_valid(&json!({"source":[]})), "{schema}");
                assert!(validate(&schema, &bound).is_err(), "{schema}");
                assert!(validate(&schema, &optional).is_err(), "{schema}");
                validate(&schema, &BTreeMap::new()).unwrap();
            }
        }
        for schema in [
            json!({"additionalProperties":true}),
            json!({"additionalProperties":{"type":"array"}}),
            json!({"properties":{"source":{"anyOf":[false,{}]}}}),
            json!({"properties":{"source":{"not":false}}}),
            json!({"properties":{"source":{"$ref":"#/$defs/envelope"}},"$defs":{"envelope":{"type":"array","items":{"required":["payload"]}}}}),
        ] {
            validate(&schema, &bound).unwrap();
        }
    }

    #[test]
    fn single_branch_one_of_preserves_symbolic_value_validation() {
        let bound = BTreeMap::from([("source".into(), Some(true))]);
        let optional = BTreeMap::from([("source".into(), None)]);
        for schema in [
            json!({"properties":{"source":{"oneOf":[{"type":"array"}]}}}),
            json!({"oneOf":[{"properties":{"source":{"type":"array"}}}]}),
            json!({"properties":{"source":{"oneOf":[{"$ref":"#/$defs/array"}]}},"$defs":{"array":{"type":"array"}}}),
        ] {
            let runtime = af_runner::task::schema_validator(&schema).unwrap();
            assert!(runtime.is_valid(&json!({"source":[]})), "{schema}");
            assert!(!runtime.is_valid(&json!({"source":42})), "{schema}");
            validate(&schema, &bound).unwrap();
            validate(&schema, &optional).unwrap();
        }
        for schema in [
            json!({"oneOf":[{"required":["missing"]}]}),
            json!({"properties":{"source":{"oneOf":[false]}}}),
            json!({"properties":{"source":{"oneOf":[{"not":{}}]}}}),
        ] {
            assert!(validate(&schema, &bound).is_err(), "{schema}");
        }
    }

    #[test]
    fn one_of_requires_proven_exclusive_acceptance() {
        let bound = BTreeMap::from([("source".into(), Some(true))]);
        for schema in [
            json!({"oneOf":[{"minProperties":1,"properties":{"source":true}},{"minProperties":1,"properties":{"source":true}}]}),
            json!({"oneOf":[{"properties":{"source":{"type":"array"}}},{"properties":{"source":{"type":"array"}}}]}),
            json!({"oneOf":[{"maxProperties":1,"additionalProperties":true},{}]}),
        ] {
            assert!(
                !af_runner::task::schema_validator(&schema)
                    .unwrap()
                    .is_valid(&json!({"source":[]}))
            );
            assert!(validate(&schema, &bound).is_err(), "{schema}");
        }
        let disjoint = json!({"oneOf":[{"maxProperties":0,"additionalProperties":true},{"minProperties":1,"properties":{"source":true}}]});
        validate(&disjoint, &bound).unwrap();
        validate(&disjoint, &BTreeMap::new()).unwrap();
        assert!(validate(&disjoint, &BTreeMap::from([("source".into(), None)])).is_err());
    }

    #[test]
    fn root_counts_and_name_constraints_respect_uncertain_supply() {
        let absent = BTreeMap::new();
        let bound = BTreeMap::from([("requirements".into(), Some(true))]);
        let optional = BTreeMap::from([("requirements".into(), None)]);
        for schema in [
            json!({"minProperties":1}),
            json!({"allOf":[{"minProperties":1}]}),
            json!({"$defs":{"count":{"minProperties":1}},"$ref":"#/$defs/count"}),
            json!({"if":{"maxProperties":0},"then":false,"else":{"minProperties":1}}),
        ] {
            assert!(validate(&schema, &absent).is_err());
            assert!(validate(&schema, &optional).is_err());
            validate(&schema, &bound).unwrap();
        }
        validate(&json!({"maxProperties":0}), &absent).unwrap();
        assert!(validate(&json!({"maxProperties":0}), &bound).is_err());
        assert!(validate(&json!({"maxProperties":0}), &optional).is_err());
        for schema in [
            json!({"additionalProperties":false}),
            json!({"properties":{"requirements":false}}),
            json!({"patternProperties":{"^req":false}}),
            json!({"propertyNames":{"const":"source"}}),
            json!({"unevaluatedProperties":false}),
            json!({"unknownPresenceConstraint":true}),
            json!({"const":{}}),
        ] {
            assert!(validate(&schema, &bound).is_err());
        }
        validate(
            &json!({"properties":{"requirements":{}},"additionalProperties":false}),
            &bound,
        )
        .unwrap();
        validate(
            &json!({"patternProperties":{"^req":{}},"additionalProperties":false}),
            &bound,
        )
        .unwrap();
        validate(&json!({"propertyNames":{"const":"requirements"}}), &bound).unwrap();
        validate(
            &json!({"anyOf":[{"maxProperties":0},{"minProperties":1}]}),
            &absent,
        )
        .unwrap();
    }

    #[test]
    fn requirements_presence_respects_composition_references_and_symbolic_branches() {
        let absent = BTreeMap::new();
        let bound = BTreeMap::from([("requirements".into(), Some(true))]);
        let optional = BTreeMap::from([("requirements".into(), None)]);
        for schema in [
            json!({"required":["requirements"]}),
            json!({"allOf":[{"required":["requirements"]}]}),
            json!({"$defs":{"r":{"required":["requirements"]}},"$ref":"#/$defs/r"}),
        ] {
            assert!(validate(&schema, &absent).is_err());
            assert!(validate(&schema, &optional).is_err());
            validate(&schema, &bound).unwrap();
        }
        validate(
            &json!({"anyOf":[{}, {"required":["requirements"]}]}),
            &absent,
        )
        .unwrap();
        validate(
            &json!({"properties":{"requirements":{"items":{"required":["payload"]}}}}),
            &bound,
        )
        .unwrap();
        let conditional = json!({"if":{"properties":{"requirements":{"const":[]}}},"then":{"required":["missing"]}});
        assert!(validate(&conditional, &bound).is_err());
        assert!(
            validate(
                &json!({"$defs":{"r":{"$ref":"#/$defs/r"}},"$ref":"#/$defs/r"}),
                &absent
            )
            .is_err()
        );
        assert!(validate(&json!({"$ref":"https://example.com/schema"}), &absent).is_err());
    }
}
