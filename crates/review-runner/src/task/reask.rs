//! One model call with its typed reply admitted tolerantly and, once, re-asked (ADR-0202).
//!
//! A reply that does not parse is first read for the first balanced JSON object it carries
//! between prose; a reply with none, or whose object is not a valid reply either, is kept whole in
//! the CAS and the same Worker is asked once more: the exact captured request, plus one feedback
//! value — `af/WorkerReplyReask@1` — that quotes the parse error, its line and column and the
//! offending excerpt. The re-ask spends the same Attempt: its remaining wall, its reservation and
//! its cancellation. Its usage adds to the first call's; nothing is reset or refunded.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use review_core::Producer;
use review_core::task::feedback::{TaskFeedbackCodeV1, WORKER_REPLY_REASK_V1, WorkerReplyReaskV1};
use review_core::task::provider_event::ProviderEventV1;
use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use review_store::Cas;

use super::{
    MAX_WORKER_BYTES, ModelWorkerReturn, WorkerAccess, WorkerContract, WorkerModelAdapter,
    WorkerReply, WorkerRequest, WorkerReturn, WorkerValue,
};

/// What the re-ask asks for, in the Worker's own terms.
const REASK_INSTRUCTION: &str = "Your previous reply did not parse as the typed reply this \
request asks for. Return it once more as exactly one JSON object, as reply_format says, with \
nothing before or after it and no markdown fence. `error` is the parser's message and `excerpt` \
the part of your reply where it stopped.";

/// The least wall worth starting a re-ask with: below it the call cannot answer.
const MIN_REASK_WALL: Duration = Duration::from_secs(1);

/// Why a reply was refused, as the re-ask and the diagnostic quote it.
struct Refusal {
    error: String,
    line: Option<u64>,
    column: Option<u64>,
    excerpt: String,
}

/// The first balanced JSON object of `bytes` when prose surrounds it: the span from its first
/// `{` to the brace that closes it, strings and escapes respected. None when there is no such
/// object, or nothing but whitespace around it (then the strict parse already judged it).
pub fn extract_object(bytes: &[u8]) -> Option<&[u8]> {
    let start = bytes.iter().position(|b| *b == b'{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes[start..].iter().enumerate() {
        if in_string {
            match (escaped, byte) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let end = start + offset + 1;
                    let around = bytes[..start]
                        .iter()
                        .chain(&bytes[end..])
                        .any(|b| !b.is_ascii_whitespace());
                    return around.then_some(&bytes[start..end]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The sum of two calls' reported usage: each counter both reported, or the one reported.
pub fn merge_usage(
    first: Option<TaskTokenUsageV3>,
    second: Option<TaskTokenUsageV3>,
) -> Option<TaskTokenUsageV3> {
    let add = |a: Option<review_core::task::usage::DecimalU128>,
               b: Option<review_core::task::usage::DecimalU128>| match (a, b) {
        (Some(a), Some(b)) => Some((a.get() + b.get()).into()),
        (a, b) => a.or(b),
    };
    match (first, second) {
        (Some(a), Some(b)) => Some(TaskTokenUsageV3 {
            input_tokens: add(a.input_tokens, b.input_tokens),
            output_tokens: add(a.output_tokens, b.output_tokens),
            cache_read_tokens: add(a.cache_read_tokens, b.cache_read_tokens),
            cache_write_tokens: add(a.cache_write_tokens, b.cache_write_tokens),
            reasoning_tokens: add(a.reasoning_tokens, b.reasoning_tokens),
            chargeable_tokens: (a.chargeable_tokens.get() + b.chargeable_tokens.get()).into(),
        }),
        (a, b) => a.or(b),
    }
}

/// The reply strictly, else the first balanced object of a reply with prose around it, else why
/// neither is a reply.
fn admit(contract: &WorkerContract, bytes: &[u8]) -> Result<WorkerReply, Refusal> {
    let strict = match contract.validate_reply(bytes) {
        Ok(reply) => return Ok(reply),
        Err(error) => error,
    };
    if let Some(object) = extract_object(bytes)
        && let Ok(reply) = contract.validate_reply(object)
    {
        return Ok(reply);
    }
    let syntax = serde_json::from_slice::<serde_json::Value>(bytes).err();
    let (line, column) = match &syntax {
        Some(error) if error.line() > 0 => (Some(error.line() as u64), Some(error.column() as u64)),
        _ => (None, None),
    };
    Err(Refusal {
        error: strict.chars().take(2048).collect(),
        line,
        column,
        excerpt: excerpt(bytes, line, column),
    })
}

/// At most 512 characters of `bytes` around the 1-based `line` and `column`, or its start.
fn excerpt(bytes: &[u8], line: Option<u64>, column: Option<u64>) -> String {
    let text = String::from_utf8_lossy(bytes);
    let chars: Vec<char> = match line {
        Some(line) => text
            .split('\n')
            .nth(line.saturating_sub(1) as usize)
            .unwrap_or_default()
            .chars()
            .collect(),
        None => text.chars().collect(),
    };
    let at = column.unwrap_or(0).saturating_sub(1) as usize;
    let start = at.saturating_sub(200).min(chars.len());
    let end = (start + 400).min(chars.len());
    chars[start..end].iter().collect()
}

/// Keep `bytes` in the CAS beside the Attempt's raw evidence; its digest.
fn keep(cas: &Cas, bytes: &[u8], raw: &mut Vec<String>) -> Result<String, String> {
    let id = cas.put(bytes).map_err(|e| e.to_string())?;
    if !raw.contains(&id) {
        raw.push(id.clone());
    }
    Ok(id)
}

/// The captured request again with the re-ask as one more feedback value, and the digest of the
/// re-ask value.
fn reask_request(
    cas: &Cas,
    request: &[u8],
    refusal: &Refusal,
    raw_reply_id: &str,
) -> Result<Vec<u8>, String> {
    let mut request: WorkerRequest =
        serde_json::from_slice(request).map_err(|_| "the request is not a typed request")?;
    let reask = WorkerReplyReaskV1 {
        schema: WORKER_REPLY_REASK_V1.into(),
        instruction: REASK_INSTRUCTION.into(),
        error: refusal.error.clone(),
        line: refusal.line,
        column: refusal.column,
        excerpt: refusal.excerpt.clone(),
        raw_reply_id: raw_reply_id.into(),
    };
    reask.validate()?;
    let payload = serde_json::to_value(&reask).map_err(|e| e.to_string())?;
    let (artifact_id, _) = cas
        .put_artifact(
            WORKER_REPLY_REASK_V1,
            Producer::KernelOperation {
                run_id: "worker-reply-reask-v1".into(),
                node_id: None,
                operation_id: "reask@1".into(),
            },
            vec![raw_reply_id.into()],
            None,
            payload.clone(),
        )
        .map_err(|e| e.to_string())?;
    request.feedback.push(WorkerValue {
        artifact_id,
        artifact_type: WORKER_REPLY_REASK_V1.into(),
        snapshot_id: None,
        payload,
    });
    let bytes = request.rendered()?;
    if bytes.len() > MAX_WORKER_BYTES {
        return Err("the re-ask would exceed the Worker byte bound".into());
    }
    Ok(bytes)
}

/// Why the first refused reply is not re-asked, or None when it is.
fn no_reask(
    returned: &ModelWorkerReturn,
    reserved_tokens: u64,
    remaining: Option<Duration>,
    cancellation: Option<&AtomicBool>,
) -> Option<&'static str> {
    if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Some("the Attempt was cancelled");
    }
    if remaining.is_none_or(|left| left < MIN_REASK_WALL) {
        return Some("the Attempt's wall is spent");
    }
    if returned.usage_observation.is_some() || returned.usage.is_none() {
        return Some("the first call's usage was not completely reported");
    }
    if returned
        .usage
        .as_ref()
        .is_some_and(|usage| usage.chargeable_tokens.get() >= u128::from(reserved_tokens))
    {
        return Some("the Attempt's token reservation is spent");
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn invoke(
    cas: &Cas,
    workdir: &Path,
    adapter: &dyn WorkerModelAdapter,
    contract: &WorkerContract,
    request: Vec<u8>,
    timeout: Duration,
    access: WorkerAccess,
    cancellation: Option<&AtomicBool>,
    reserved_tokens: u64,
) -> WorkerReturn {
    let started = Instant::now();
    let first = adapter.invoke(
        cas,
        workdir,
        request.clone(),
        timeout,
        access,
        cancellation,
        &[],
    );
    let message = match &first.message {
        Ok(message) => message.clone(),
        Err(error) => {
            return WorkerReturn {
                reply: Err(error.clone()),
                feedback_code: Some(TaskFeedbackCodeV1::ProviderFailure),
                usage_observation: first.usage_observation,
                usage: first.usage,
                raw_artifact_ids: first.raw_artifact_ids,
                native_failure: first.native_failure,
                notes: first.notes,
            };
        }
    };
    let refusal = match admit(contract, &message) {
        Ok(reply) => {
            return WorkerReturn {
                reply: Ok(reply),
                feedback_code: None,
                usage_observation: first.usage_observation,
                usage: first.usage,
                raw_artifact_ids: first.raw_artifact_ids,
                native_failure: first.native_failure,
                notes: first.notes,
            };
        }
        Err(refusal) => refusal,
    };
    let mut raw = first.raw_artifact_ids.clone();
    let failed = |raw: Vec<String>, first: ModelWorkerReturn, error: String| WorkerReturn {
        reply: Err(error),
        feedback_code: Some(TaskFeedbackCodeV1::InvalidOutputContract),
        usage_observation: first.usage_observation,
        usage: first.usage,
        raw_artifact_ids: raw,
        native_failure: first.native_failure,
        notes: first.notes,
    };
    let raw_reply_id = match keep(cas, &message, &mut raw) {
        Ok(id) => id,
        Err(error) => {
            let error = format!(
                "{}; the raw reply could not be kept: {error}",
                refusal.error
            );
            return failed(raw, first, error);
        }
    };
    let kept = format!("{}; raw reply kept: {raw_reply_id}", refusal.error);
    let remaining = timeout.checked_sub(started.elapsed());
    if let Some(why) = no_reask(&first, reserved_tokens, remaining, cancellation) {
        return failed(raw, first, format!("{kept}; not re-asked: {why}"));
    }
    let reask = match reask_request(cas, &request, &refusal, &raw_reply_id) {
        Ok(reask) => reask,
        Err(why) => return failed(raw, first, format!("{kept}; not re-asked: {why}")),
    };
    let reask_id = match keep(cas, &reask, &mut raw) {
        Ok(id) => id,
        Err(why) => return failed(raw, first, format!("{kept}; not re-asked: {why}")),
    };
    let remaining = remaining.expect("checked above");
    let second = adapter.invoke(cas, workdir, reask, remaining, access, cancellation, &[]);
    for id in &second.raw_artifact_ids {
        if !raw.contains(id) {
            raw.push(id.clone());
        }
    }
    // The two calls are one Attempt's spend: their counters add. A second call that reported
    // nothing leaves the Attempt's billing incomplete, never charged as the first alone.
    let reported = merge_usage(
        first.usage.clone(),
        second
            .usage
            .clone()
            .or_else(|| second.usage_observation.as_ref()?.reported_usage.clone()),
    );
    let complete = second.usage_observation.is_none() && second.usage.is_some()
        || second
            .usage_observation
            .as_ref()
            .is_some_and(|observation| observation.charge_complete);
    let usage_observation =
        (!complete || second.usage_observation.is_some()).then(|| TaskUsageObservationV1 {
            reported_usage: reported.clone(),
            charge_complete: complete,
        });
    let mut notes = first.notes;
    notes.events.push(ProviderEventV1::ReplyReasked {
        raw_reply_id: raw_reply_id.clone(),
        reask_id,
    });
    let notes = notes.then(second.notes);
    let (reply, feedback_code) = match second.message {
        Ok(message) => match admit(contract, &message) {
            Ok(reply) => (Ok(reply), None),
            Err(again) => {
                let second_id = keep(cas, &message, &mut raw)
                    .unwrap_or_else(|error| format!("(not kept: {error})"));
                (
                    Err(format!(
                        "{kept}; re-asked once in the same Attempt, and the second reply did not \
                         parse either: {}; raw reply kept: {second_id}",
                        again.error
                    )),
                    Some(TaskFeedbackCodeV1::InvalidOutputContract),
                )
            }
        },
        Err(error) => (
            Err(format!(
                "{kept}; re-asked once in the same Attempt, and the re-ask failed: {error}"
            )),
            Some(TaskFeedbackCodeV1::ProviderFailure),
        ),
    };
    WorkerReturn {
        reply,
        feedback_code,
        usage_observation,
        usage: reported,
        raw_artifact_ids: raw,
        native_failure: second.native_failure,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_balanced_object_is_extracted_only_from_prose_around_it() {
        let reply = br#"Here is my answer: {"schema":"af.worker-reply/1","outputs":{"a":[{"t":"a } and { inside \" a string"}]}} Hope that helps {"x":1}"#;
        let object = extract_object(reply).unwrap();
        let value: serde_json::Value = serde_json::from_slice(object).unwrap();
        assert_eq!(
            value["outputs"]["a"][0]["t"],
            "a } and { inside \" a string"
        );
        assert_eq!(
            extract_object(br#"  {"a":1}  "#),
            None,
            "no prose around it"
        );
        assert_eq!(extract_object(b"no object here"), None);
        assert_eq!(extract_object(br#"unbalanced {"a":[1,2"#), None);
    }

    #[test]
    fn usage_of_two_calls_adds_counter_by_counter() {
        let a = TaskTokenUsageV3 {
            input_tokens: Some(10u128.into()),
            output_tokens: Some(5u128.into()),
            cache_read_tokens: None,
            cache_write_tokens: Some(1u128.into()),
            reasoning_tokens: None,
            chargeable_tokens: 16u128.into(),
        };
        let b = TaskTokenUsageV3 {
            input_tokens: Some(7u128.into()),
            output_tokens: Some(3u128.into()),
            cache_read_tokens: Some(4u128.into()),
            cache_write_tokens: None,
            reasoning_tokens: None,
            chargeable_tokens: 10u128.into(),
        };
        let sum = merge_usage(Some(a.clone()), Some(b)).unwrap();
        assert_eq!(sum.input_tokens.unwrap().get(), 17);
        assert_eq!(sum.output_tokens.unwrap().get(), 8);
        assert_eq!(sum.cache_read_tokens.unwrap().get(), 4);
        assert_eq!(sum.cache_write_tokens.unwrap().get(), 1);
        assert_eq!(sum.chargeable_tokens.get(), 26);
        assert_eq!(merge_usage(Some(a.clone()), None), Some(a));
        assert_eq!(merge_usage(None, None), None);
    }

    #[test]
    fn the_excerpt_is_the_failing_line_around_its_column() {
        let mut reply = "x".repeat(1000);
        reply.push_str("BROKEN");
        reply.push_str(&"y".repeat(1000));
        let around = excerpt(reply.as_bytes(), Some(1), Some(1001));
        assert!(around.contains("BROKEN"), "{around}");
        assert!(around.chars().count() <= 512);
        assert_eq!(excerpt(b"short", None, None), "short");
    }
}
