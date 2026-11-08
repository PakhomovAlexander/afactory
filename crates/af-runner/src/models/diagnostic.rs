//! Provider-neutral binding diagnostics and grammar G (ADR-0147, ADR-0148).
//!
//! Static and catalog refusals share one formatter with plan-time and runtime consumers.
//! Every printed identifier passes grammar G: no secret, prompt, Worker text or path is echoed.
//! Claude's native refusal, model-switch and unavailable-model hints remain in its adapter.

use super::ProviderRef;

/// The model the Worker bound, exactly as the adapter passed it to its harness.
#[derive(Clone, Copy, Debug)]
pub struct BoundModel<'a> {
    pub model: &'a str,
    /// `--effort`, when the binding carries one.
    pub effort: Option<&'a str>,
    /// The Provider the binding runs on, when the caller knows it.
    pub provider: Option<ProviderRef<'a>>,
}

impl BoundModel<'_> {
    /// The bound model and optional effort, rendered only through grammar G.
    pub fn with_effort(self) -> String {
        let model = shown(self.model);
        match self.effort {
            Some(effort) => format!("{model} (effort {})", shown(effort)),
            None => model,
        }
    }
}

/// The next step for a binding af refused.
pub const NOT_ACCEPTED_NEXT: &str = "next: bind an exact model ID and an effort level this \
Provider lists, then rerun; only the charged `af provider doctor` proves that the Provider runs \
it";

/// The opening words of a binding refusal, also used to find it in an Attempt diagnostic.
pub const BINDING_OPENER: &str = "af refused the binding of ";

/// The hint for a binding af refused before any process started: its rule or catalog fact,
/// nearest accepted spellings and fixed next step. Native Provider failures stay in adapters.
pub fn compose_binding(bound: BoundModel<'_>, fact: &str, nearest: &[String]) -> String {
    let with_effort = bound.with_effort();
    let provider = bound
        .provider
        .map(provider_words)
        .unwrap_or_else(|| "its Provider".into());
    // A package may bind an effort level and leave the model to the harness.
    let subject = if bound.model.is_empty() {
        match bound.effort {
            Some(effort) => {
                format!("the harness's default model (effort {})", shown(effort))
            }
            None => "the harness's default model".into(),
        }
    } else {
        format!("model {with_effort}")
    };
    format!(
        "af refused the binding of {subject} for {provider} (model_not_accepted): {fact}; \
         {}{}; {NOT_ACCEPTED_NEXT}",
        nearest_words(nearest),
        spellings_words(bound.provider)
    )
}

/// `Provider ID (KIND)`, or `a KIND Provider` when only the kind is known.
pub fn provider_words(provider: ProviderRef<'_>) -> String {
    match provider.id {
        Some(id) => format!("Provider {} ({})", shown(id), provider.kind.name()),
        None => format!("a {} Provider", provider.kind.name()),
    }
}

/// `nearest accepted spellings: A, B; ` when there are any.
pub fn nearest_words(nearest: &[String]) -> String {
    if nearest.is_empty() {
        return String::new();
    }
    let listed = nearest
        .iter()
        .map(|value| shown(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("nearest accepted spellings: {listed}; ")
}

/// The command that lists the spellings the Provider accepts.
pub fn spellings_words(provider: Option<ProviderRef<'_>>) -> String {
    let command = match provider.and_then(|provider| provider.id) {
        Some(id) if printable(id) => format!("af provider models {id}"),
        _ => "af provider models".into(),
    };
    format!("the spellings this Provider accepts: `{command}` (token-free)")
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

pub(super) fn byte_length(bytes: usize) -> String {
    match bytes {
        1 => "1 byte".into(),
        n => format!("{n} bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FABLE: BoundModel<'static> = BoundModel {
        model: "claude-fable-5-1",
        effort: Some("xhigh"),
        provider: None,
    };

    #[test]
    fn the_attempt_line_names_node_provider_model_effort_and_hint() {
        let hint = compose_binding(FABLE, "a fact", &[]);
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
}
