//! Raw Responses usage, captured at the gateway, never from app-server token events.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledUsage {
    pub input: u64,
    pub cached_input: u64,
    pub output: u64,
    pub reasoning_output: Option<u64>,
    pub inclusive: u64,
    pub chargeable: u64,
}

/// Presence is explicit on the wire: absent and a present JSON null round-trip distinctly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "presence",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RawField {
    Absent,
    Present(Value),
}

/// Invalid native values remain intact, independently of reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawUsage {
    pub observations: Vec<Value>,
    pub terminal_event: Option<String>,
    pub value: RawField,
    pub reconciled: Option<ReconciledUsage>,
    pub failure: Option<String>,
}

fn reconcile(value: &Value) -> Option<ReconciledUsage> {
    let input = value.get("input_tokens")?.as_u64()?;
    let output = value.get("output_tokens")?.as_u64()?;
    let cached_input = value
        .pointer("/input_tokens_details/cached_tokens")?
        .as_u64()?;
    let reasoning = value.pointer("/output_tokens_details/reasoning_tokens");
    let reasoning_output = match reasoning {
        None => None,
        Some(value) => Some(value.as_u64()?),
    };
    if cached_input > input || reasoning_output.is_some_and(|n| n > output) {
        return None;
    }
    let inclusive = input.checked_add(output)?;
    if value
        .get("total_tokens")
        .is_some_and(|n| n.as_u64() != Some(inclusive))
    {
        return None;
    }
    Some(ReconciledUsage {
        input,
        cached_input,
        output,
        reasoning_output,
        inclusive,
        chargeable: input.checked_sub(cached_input)?.checked_add(output)?,
    })
}

/// Both usage capture and tool validation must inspect the same assembled SSE events.
/// Splitting individual data lines would miss valid multiline JSON tool calls.
pub(super) fn data_frames(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split("\n\n").filter_map(|frame| {
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        (!data.is_empty()).then_some(data)
    })
}

/// Parse a complete bounded SSE body. Completion alone is insufficient: EOF is required,
/// so duplicate/late terminal data cannot race the next inference. No normalization occurs.
pub(super) fn capture(body: &[u8]) -> RawUsage {
    let mut raw = RawUsage {
        observations: Vec::new(),
        terminal_event: None,
        value: RawField::Absent,
        reconciled: None,
        failure: None,
    };
    let Ok(text) = std::str::from_utf8(body) else {
        raw.failure = Some("non-UTF8 SSE".into());
        return raw;
    };
    let text = text.replace("\r\n", "\n");
    let mut done = false;
    for data in data_frames(&text) {
        if data == "[DONE]" {
            if done || raw.terminal_event.is_none() {
                raw.failure = Some("contradictory SSE end marker".into());
            }
            done = true;
            continue;
        }
        if done {
            raw.failure = Some("data after SSE end marker".into());
        }
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            raw.failure = Some("malformed SSE JSON".into());
            continue;
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        let terminal = matches!(
            kind,
            "response.completed" | "response.failed" | "response.incomplete"
        );
        if terminal || event.get("usage").is_some() || event.pointer("/response/usage").is_some() {
            raw.observations.push(event.clone());
        }
        if raw.terminal_event.is_some() {
            raw.failure = Some("data after terminal usage".into());
            continue;
        }
        // Usage outside the terminal response is contradictory, even if numerically equal.
        if !terminal && (event.get("usage").is_some() || event.pointer("/response/usage").is_some())
        {
            raw.failure = Some("nonterminal usage".into());
        }
        if terminal {
            raw.terminal_event = Some(kind.into());
            raw.value = event
                .pointer("/response/usage")
                .cloned()
                .map(RawField::Present)
                .unwrap_or(RawField::Absent);
            raw.reconciled = match &raw.value {
                RawField::Present(value) => reconcile(value),
                RawField::Absent => None,
            };
            if raw.reconciled.is_none() {
                raw.failure = Some("unknown or malformed raw usage".into());
            }
            if kind == "response.completed"
                && event.pointer("/response/status").and_then(Value::as_str) != Some("completed")
            {
                raw.failure = Some("contradictory terminal status".into());
            }
            if kind != "response.completed" {
                raw.failure = Some("upstream terminal failure".into());
            }
        }
    }
    if raw.terminal_event.is_none() {
        raw.failure = Some("missing terminal raw usage".into());
    }
    // A trailing unfinished frame is not a confirmed terminal stream.
    if !text.ends_with("\n\n") {
        raw.failure = Some("unterminated SSE frame".into());
    }
    raw
}
