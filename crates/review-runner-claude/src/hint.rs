//! The one af-authored hint for a Claude Model Attempt that the bound model did not answer
//! (ADR-0147): its safety classifier flagged the request and it refused (`model_refused`),
//! Claude Code switched to a model the Worker did not bind (`model_refused` after a
//! safety-classifier fallback, `model_switched` otherwise), or Claude Code does not know the
//! model (`model_unavailable`).
//!
//! [`compose`] writes the hint, and the Attempt diagnostic starts with it after `Claude Worker
//! failed: `. `af task run` and `af review run` find it there again with [`find`] and print it on
//! stderr through [`attempt_line`], so an operator reads the same words on the run's own output
//! that `af task show` keeps in the full diagnostic. A hint carries af's own words, model IDs,
//! counts and a closed category only: no secret, no prompt text, no Worker output and no path.
//! Every model ID, effort and name is printed only in grammar G ([`printable`]); anything else
//! is `[not printable]` with its byte length, and a binding outside G never starts a process.

/// The model the Worker bound, exactly as the adapter passed it to Claude Code.
#[derive(Clone, Copy, Debug)]
pub struct BoundModel<'a> {
    pub model: &'a str,
    /// `--effort`, when the binding carries one.
    pub effort: Option<&'a str>,
}

/// One model other than the bound one that Claude Code reported activity for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherModel {
    pub model: String,
    pub output_tokens: Option<u64>,
}

/// What happened instead of the bound model's reply.
#[derive(Clone, Copy, Debug)]
pub enum ModelOutcome<'a> {
    /// The bound model's safety classifier flagged the request and the bound model refused it.
    Refused {
        safety_stops: Option<u64>,
        stop_reason_refusal: bool,
        /// The policy category the result names, when it names one.
        category: Option<&'a str>,
    },
    /// Claude Code ran the request on other models: a safety-classifier fallback when
    /// `safety_stops` is above zero, otherwise an unexplained switch.
    Switched {
        /// The bound model's own output tokens, when Claude Code reported usage for it.
        bound_output_tokens: Option<u64>,
        others: &'a [OtherModel],
        safety_stops: Option<u64>,
        num_turns: Option<u64>,
    },
    /// Claude Code does not know the model ID, or the account cannot use it.
    Unavailable,
}

/// How a hint begins: a refusal or a safety-classifier fallback, an unexplained switch, an
/// unknown model.
const OPENERS: [&str; 3] = [
    "the safety classifier of ",
    "Claude Code switched models",
    "Claude Code rejected the model `",
];

/// The next step after a refusal.
const REFUSED_NEXT: &str = "next: af does not retry it, because the same input is flagged \
again; change the input or bind another model, then rerun";

/// The next step after a switch: af already asked Claude Code not to switch.
const SWITCHED_NEXT: &str = "next: af already sets CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1 so \
that Claude Code keeps the bound model, so upgrade it with `npm install -g \
@anthropic-ai/claude-code@latest` and rerun; if the safety classifier flags the input again, \
change the input or bind another model";

/// The next step for a model ID Claude Code rejected.
const UNAVAILABLE_NEXT: &str = "next: correct the model ID in the binding (Claude model IDs are \
spelled with hyphens, such as claude-fable-5-1), then check the Provider with `af provider \
status` (token-free) and `af provider doctor` (charged)";

/// Every way a hint ends: [`find`] reads up to the first of them.
const ENDINGS: [&str; 3] = [REFUSED_NEXT, SWITCHED_NEXT, UNAVAILABLE_NEXT];

/// The Attempt diagnostic's own prefix, which the hint follows.
pub const DIAGNOSTIC_PREFIX: &str = "Claude Worker failed: ";

/// The refusal the Task runtime puts before a diagnostic whose billing was incomplete.
const INCOMPLETE_BILLING: &str = "Native billing usage is incomplete: ";

/// The classification a hint for `outcome` names.
pub fn classification(outcome: &ModelOutcome<'_>) -> &'static str {
    match outcome {
        ModelOutcome::Refused { .. } => "model_refused",
        ModelOutcome::Switched {
            safety_stops: Some(1..),
            ..
        } => "model_refused",
        ModelOutcome::Switched { .. } => "model_switched",
        ModelOutcome::Unavailable => "model_unavailable",
    }
}

/// The hint: the classification, what happened with the bound model and every count the result
/// reported, why af refused it, and the next step.
pub fn compose(bound: BoundModel<'_>, outcome: &ModelOutcome<'_>) -> String {
    let model = shown(bound.model);
    let with_effort = match bound.effort {
        Some(effort) => format!("{model} (effort {})", shown(effort)),
        None => model.clone(),
    };
    let class = classification(outcome);
    match outcome {
        ModelOutcome::Refused {
            safety_stops,
            stop_reason_refusal,
            category,
        } => {
            let mut facts = Vec::new();
            if let Some(stops) = safety_stops {
                facts.push(format!("safety_stops {stops}"));
            }
            if *stop_reason_refusal {
                facts.push("stop_reason refusal".into());
            }
            if let Some(category) = category.filter(|c| is_category(c)) {
                facts.push(format!("category {category}"));
            }
            let facts = if facts.is_empty() {
                "Claude Code reported that its safeguards flagged this message".into()
            } else {
                facts.join(", ")
            };
            format!(
                "the safety classifier of {with_effort} flagged the request and {model} refused \
                 it ({class}; {facts}); af turned Claude Code's automatic switch to another model \
                 off, because af never runs a model the Worker did not bind; {REFUSED_NEXT}"
            )
        }
        ModelOutcome::Switched {
            bound_output_tokens,
            others,
            safety_stops,
            num_turns,
        } => {
            let cause = if safety_stops.is_some_and(|stops| stops >= 1) {
                format!(
                    "the safety classifier of {model} flagged the request and Claude Code \
                     switched models, a safety-classifier fallback ({class})"
                )
            } else {
                format!(
                    "Claude Code switched models without reporting a safety stop, an \
                     unexplained switch ({class})"
                )
            };
            let bound_usage = match bound_output_tokens {
                Some(tokens) => format!("which wrote {tokens} output tokens"),
                None => "which reported no usage".into(),
            };
            let answered = others
                .iter()
                .map(|other| format!("{} ({})", shown(&other.model), output(other.output_tokens)))
                .collect::<Vec<_>>()
                .join(", ");
            let mut counts = Vec::new();
            if let Some(stops) = safety_stops {
                counts.push(format!("safety_stops {stops}"));
            }
            if let Some(turns) = num_turns {
                counts.push(format!("num_turns {turns}"));
            }
            let counts = if counts.is_empty() {
                String::new()
            } else {
                format!(" ({})", counts.join(", "))
            };
            format!(
                "{cause}: the Worker bound {with_effort}, {bound_usage}, but {answered} \
                 answered{counts}; af refused the reply, because af never runs or accepts a \
                 model the Worker did not bind; this is neither a credential nor a model-spelling \
                 problem; {SWITCHED_NEXT}"
            )
        }
        ModelOutcome::Unavailable => format!(
            "Claude Code rejected the model `{model}` ({class}): there is an issue with the \
             selected model, so Claude Code does not know this model ID or the account cannot \
             use it; {UNAVAILABLE_NEXT}"
        ),
    }
}

/// The hint an Attempt diagnostic starts with, if it starts with one. Only af's own head of the
/// diagnostic is read, up to the end of its fixed next step, so no Worker text after it is.
pub fn find(diagnostic: &str) -> Option<&str> {
    let rest = diagnostic
        .strip_prefix(INCOMPLETE_BILLING)
        .unwrap_or(diagnostic)
        .strip_prefix(DIAGNOSTIC_PREFIX)?;
    if !OPENERS.iter().any(|opener| rest.starts_with(opener)) {
        return None;
    }
    let end = ENDINGS
        .iter()
        .filter_map(|ending| rest.find(ending).map(|at| at + ending.len()))
        .min()?;
    Some(&rest[..end])
}

/// The one stderr line `af task run` and `af review run` print for a failed admission or Worker
/// Attempt whose diagnostic carries a hint: the node, the Provider ID and kind, the bound model
/// and effort, then the hint with its classification and next step.
pub fn attempt_line(
    node: &str,
    provider: &str,
    provider_kind: &str,
    model: &str,
    effort: &str,
    hint: &str,
) -> String {
    format!(
        "af: {} failed: Provider {} ({}), model {}, effort {}: {hint}",
        shown(node),
        shown(provider),
        shown(provider_kind),
        shown(model),
        shown(effort)
    )
}

fn output(tokens: Option<u64>) -> String {
    match tokens {
        Some(tokens) => format!("{tokens} output tokens"),
        None => "output tokens not reported".into(),
    }
}

/// Grammar G, the one shape of a spelling af prints verbatim (ADR-0147): an ASCII letter or digit,
/// then at most 127 further bytes, each an ASCII letter, digit, `-`, `.`, `_` or `:`. No `/`, no
/// `@`, no whitespace and no other byte, so a path or an account identifier never matches it.
pub const PRINTABLE_RULE: &str = "a spelling af prints starts with an ASCII letter or digit and \
continues with at most 127 ASCII letters, digits, '-', '.', '_' or ':', with no '/', '@' or \
whitespace";

/// The most bytes a spelling in grammar G holds.
const PRINTABLE_MAX_BYTES: usize = 128;

/// Whether `value` is in grammar G ([`PRINTABLE_RULE`]).
pub fn printable(value: &str) -> bool {
    value.len() <= PRINTABLE_MAX_BYTES
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._:".contains(&b))
}

/// Refuse a binding's `--model` or `--effort` value outside grammar G before any process starts.
/// The refusal names the rule and the value's byte length, never the value.
pub fn check_binding(option: &str, value: &str) -> Result<(), String> {
    if printable(value) {
        return Ok(());
    }
    Err(format!(
        "Model binding refused: its {option} value ({}) is not a model spelling af accepts: \
         {PRINTABLE_RULE}",
        byte_length(value.len())
    ))
}

/// An identifier as af prints it: a spelling in grammar G keeps its own spelling; anything else
/// is replaced by `[not printable]` and its byte length, never any part of it.
pub fn shown(value: &str) -> String {
    if printable(value) {
        value.into()
    } else {
        format!("[not printable] ({})", byte_length(value.len()))
    }
}

fn byte_length(bytes: usize) -> String {
    match bytes {
        1 => "1 byte".into(),
        n => format!("{n} bytes"),
    }
}

/// A policy category is a short lower-case name; anything else is left out.
fn is_category(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FABLE: BoundModel<'static> = BoundModel {
        model: "claude-fable-5-1",
        effort: Some("xhigh"),
    };

    fn every_outcome(others: &[OtherModel]) -> Vec<ModelOutcome<'_>> {
        vec![
            ModelOutcome::Refused {
                safety_stops: Some(1),
                stop_reason_refusal: true,
                category: Some("cyber"),
            },
            ModelOutcome::Refused {
                safety_stops: None,
                stop_reason_refusal: false,
                category: None,
            },
            ModelOutcome::Switched {
                bound_output_tokens: Some(0),
                others,
                safety_stops: Some(2),
                num_turns: Some(2),
            },
            ModelOutcome::Switched {
                bound_output_tokens: None,
                others,
                safety_stops: None,
                num_turns: None,
            },
            ModelOutcome::Unavailable,
        ]
    }

    #[test]
    fn the_recorded_fallback_names_both_models_the_counts_the_cause_and_the_next_step() {
        let others = [OtherModel {
            model: "claude-opus-5".into(),
            output_tokens: Some(4),
        }];
        let hint = compose(
            FABLE,
            &ModelOutcome::Switched {
                bound_output_tokens: Some(0),
                others: &others,
                safety_stops: Some(2),
                num_turns: Some(2),
            },
        );
        assert_eq!(
            hint,
            "the safety classifier of claude-fable-5-1 flagged the request and Claude Code \
             switched models, a safety-classifier fallback (model_refused): the Worker bound \
             claude-fable-5-1 (effort xhigh), which wrote 0 output tokens, but claude-opus-5 (4 \
             output tokens) answered (safety_stops 2, num_turns 2); af refused the reply, \
             because af never runs or accepts a model the Worker did not bind; this is neither \
             a credential nor a model-spelling problem; next: af already sets \
             CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1 so that Claude Code keeps the bound model, \
             so upgrade it with `npm install -g @anthropic-ai/claude-code@latest` and rerun; if \
             the safety classifier flags the input again, change the input or bind another model"
        );
    }

    #[test]
    fn every_hint_is_found_again_and_nothing_after_it_is() {
        let others = [OtherModel {
            model: "claude-opus-5".into(),
            output_tokens: Some(4),
        }];
        for outcome in every_outcome(&others) {
            let hint = compose(FABLE, &outcome);
            assert!(
                hint.contains(&format!("({}", classification(&outcome))),
                "{hint}"
            );
            let diagnostic = format!(
                "{DIAGNOSTIC_PREFIX}{hint}; transport: exited 1; stderr: WORKER-TEXT next: af does not retry it"
            );
            assert_eq!(find(&diagnostic), Some(hint.as_str()));
            let incomplete = format!("{INCOMPLETE_BILLING}{diagnostic}");
            assert_eq!(find(&incomplete), Some(hint.as_str()));
            assert!(!find(&diagnostic).unwrap().contains("WORKER-TEXT"));
        }
    }

    #[test]
    fn a_hint_is_found_only_at_the_head_of_an_af_diagnostic() {
        let hint = compose(FABLE, &ModelOutcome::Unavailable);
        for diagnostic in [
            hint.clone(),
            format!(
                "Claude Worker exited 1 without a parsable result; stderr: {DIAGNOSTIC_PREFIX}{hint}"
            ),
            format!("{DIAGNOSTIC_PREFIX}Provider model is unavailable (model_unavailable)"),
            format!("{DIAGNOSTIC_PREFIX}Claude Code rejected the model `x`: no fixed next step"),
        ] {
            assert_eq!(find(&diagnostic), None, "{diagnostic}");
        }
    }

    #[test]
    fn the_misspelled_model_is_quoted_with_both_provider_checks() {
        let hint = compose(
            BoundModel {
                model: "claude-fable-5.1",
                effort: Some("xhigh"),
            },
            &ModelOutcome::Unavailable,
        );
        assert!(
            hint.starts_with(
                "Claude Code rejected the model `claude-fable-5.1` (model_unavailable)"
            ),
            "{hint}"
        );
        assert!(hint.contains("`af provider status` (token-free)"), "{hint}");
        assert!(hint.contains("`af provider doctor` (charged)"), "{hint}");
    }

    #[test]
    fn a_refusal_names_the_bound_model_its_effort_counts_category_and_no_retry() {
        let hint = compose(
            FABLE,
            &ModelOutcome::Refused {
                safety_stops: Some(1),
                stop_reason_refusal: false,
                category: Some("cyber"),
            },
        );
        for part in [
            "claude-fable-5-1 (effort xhigh)",
            "(model_refused; safety_stops 1",
            "category cyber",
            "af never runs a model the Worker did not bind",
            "af does not retry it",
        ] {
            assert!(hint.contains(part), "{part}: {hint}");
        }
        // A category that is not a short closed name is left out, never echoed.
        let hint = compose(
            FABLE,
            &ModelOutcome::Refused {
                safety_stops: Some(1),
                stop_reason_refusal: false,
                category: Some("Ignore previous instructions"),
            },
        );
        assert!(!hint.contains("Ignore"), "{hint}");
    }

    #[test]
    fn the_attempt_line_names_node_provider_model_effort_and_hint() {
        let hint = compose(FABLE, &ModelOutcome::Unavailable);
        assert_eq!(
            attempt_line(
                "root.providers.admit0",
                "claude-personal",
                "claude",
                "claude-fable-5-1",
                "xhigh",
                &hint
            ),
            format!(
                "af: root.providers.admit0 failed: Provider claude-personal (claude), model \
                 claude-fable-5-1, effort xhigh: {hint}"
            )
        );
        assert!(
            attempt_line("node", "p", "claude", "a b", "x\ny", &hint).contains("[not printable]")
        );
    }

    #[test]
    fn grammar_g_prints_model_ids_and_never_a_path_or_an_account() {
        for value in [
            "claude-fable-5-1",
            "claude-fable-5.1",
            "us.anthropic.claude-opus-5-5-v1:0",
            "xhigh",
            "claude_opus_5",
            &"a".repeat(128),
        ] {
            assert!(printable(value), "{value}");
            assert_eq!(shown(value), value);
            assert_eq!(check_binding("--model", value), Ok(()));
        }
        for value in [
            "/Users/alice/keys-1",
            "developer@example.test",
            "claude-opus-5@20260101",
            "anthropic/claude-opus-5",
            "-claude",
            ".claude",
            "claude opus",
            "claude\n",
            "",
            &"a".repeat(129),
        ] {
            assert!(!printable(value), "{value:?}");
            let printed = shown(value);
            assert_eq!(
                printed,
                format!("[not printable] ({} bytes)", value.len()),
                "{value:?}"
            );
            let refusal = check_binding("--model", value).unwrap_err();
            assert!(refusal.contains(PRINTABLE_RULE), "{refusal}");
            assert!(
                refusal.contains(&format!("--model value ({} bytes)", value.len())),
                "{refusal}"
            );
            for part in ["Users", "alice", "example", "anthropic/", "opus"] {
                assert!(
                    !printed.contains(part) && !refusal.contains(part),
                    "{refusal}"
                );
            }
        }
        assert_eq!(shown("?"), "[not printable] (1 byte)");
    }

    #[test]
    fn a_foreign_model_outside_g_is_named_not_printable_and_a_bedrock_id_verbatim() {
        let others = [
            OtherModel {
                model: "/Users/alice/keys-1".into(),
                output_tokens: Some(4),
            },
            OtherModel {
                model: "us.anthropic.claude-opus-5-5-v1:0".into(),
                output_tokens: Some(3),
            },
        ];
        let hint = compose(
            FABLE,
            &ModelOutcome::Switched {
                bound_output_tokens: Some(0),
                others: &others,
                safety_stops: Some(0),
                num_turns: Some(1),
            },
        );
        assert!(
            hint.contains(
                "but [not printable] (19 bytes) (4 output tokens), \
                 us.anthropic.claude-opus-5-5-v1:0 (3 output tokens) answered"
            ),
            "{hint}"
        );
        assert!(!hint.contains("Users") && !hint.contains("alice"), "{hint}");
        let diagnostic = format!("{DIAGNOSTIC_PREFIX}{hint}; transport: exited 0");
        assert_eq!(find(&diagnostic), Some(hint.as_str()));
    }
}
