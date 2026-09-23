//! Reconcile overlapping native summaries; never add top-level usage to its model breakdown.
use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use review_runner::task::usage::NativeCounter;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_MODELS: usize = 32;
const MAX_MODEL_ID_BYTES: usize = 256;

pub(super) struct Accounting {
    pub usage: Option<TaskTokenUsageV3>,
    pub observation: Option<TaskUsageObservationV1>,
    pub error: Option<&'static str>,
}

#[derive(Clone, Copy, Default)]
struct Counters {
    input: Option<u128>,
    output: Option<u128>,
    write: Option<u128>,
    read: Option<u128>,
    complete: bool,
    malformed: bool,
    charge_floor: u128,
}

impl Counters {
    fn parse(value: &Value, keys: [&str; 4]) -> Self {
        let [input, output, write, read] = keys.map(|key| NativeCounter::read(value, key));
        let complete = value.is_object()
            && input.value().is_some()
            && output.value().is_some()
            && write.optional_zero().is_some();
        Self {
            input: input.value().map(u128::from),
            output: output.value().map(u128::from),
            write: write.optional_zero().map(u128::from),
            read: read.value().map(u128::from),
            complete,
            malformed: !complete || read == NativeCounter::Invalid,
            charge_floor: 0,
        }
    }

    fn same_bill(self, other: Self) -> bool {
        self.complete
            && other.complete
            && (self.input, self.output, self.write) == (other.input, other.output, other.write)
    }

    fn read_conflicts(self, other: Self) -> bool {
        matches!((self.read, other.read), (Some(a), Some(b)) if a != b)
    }

    fn lower_bound(self, other: Self) -> Self {
        Self {
            charge_floor: self.charge().max(other.charge()),
            malformed: true,
            ..Self::default()
        }
    }

    fn charge(self) -> u128 {
        self.charge_floor
            .max(self.input.unwrap_or(0) + self.output.unwrap_or(0) + self.write.unwrap_or(0))
    }

    fn usage(self) -> Option<TaskTokenUsageV3> {
        (self.input.is_some()
            || self.output.is_some()
            || self.write.is_some()
            || self.read.is_some()
            || self.charge_floor != 0)
            .then(|| TaskTokenUsageV3 {
                input_tokens: self.input.map(Into::into),
                output_tokens: self.output.map(Into::into),
                cache_write_tokens: self.write.map(Into::into),
                cache_read_tokens: self.read.map(Into::into),
                reasoning_tokens: None,
                chargeable_tokens: self.charge().into(),
            })
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_ID_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._:/".contains(&b))
}

pub(super) fn account(value: Option<&Value>, selected: &str) -> Accounting {
    let (top_usage, top_observation) = super::parse_usage(value);
    let Some(map) = value.and_then(|v| v.get("modelUsage")) else {
        // Preserve the top-level-only native contract, including its malformed-usage observation.
        return Accounting {
            usage: top_usage,
            observation: top_observation,
            error: None,
        };
    };
    let mut error = None;
    let mut refuse = |reason| {
        error.get_or_insert(reason);
    };
    let top = value
        .and_then(|v| v.get("usage"))
        .map(|v| {
            Counters::parse(
                v,
                [
                    "input_tokens",
                    "output_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                ],
            )
        })
        .unwrap_or_default();
    let mut complete = top.complete;
    if top.malformed {
        refuse("Claude top-level usage is malformed");
    }
    let mut groups = BTreeMap::<&str, Counters>::new();
    let mut scopes = Vec::new();
    if let Some(entries) = map.as_object() {
        if entries.is_empty() || entries.len() > MAX_MODELS {
            complete = false;
            refuse("Claude model usage exceeds its finite nonempty model bound");
        }
        for (key, model) in entries.iter().take(MAX_MODELS) {
            let counters = Counters::parse(
                model,
                [
                    "inputTokens",
                    "outputTokens",
                    "cacheCreationInputTokens",
                    "cacheReadInputTokens",
                ],
            );
            complete &= counters.complete;
            if counters.malformed {
                refuse("Claude per-model usage is malformed");
            }
            let canonical = model.get("canonicalModel");
            let canonical_id = canonical.and_then(Value::as_str);
            let identity_valid = valid_id(key)
                && canonical.is_none_or(|_| canonical_id.is_some_and(valid_id))
                && !(key == selected && canonical_id.is_some_and(|id| id != selected));
            if !identity_valid {
                complete = false;
                refuse("Claude model usage identity is malformed");
            }
            let matches = identity_valid
                && if key == selected {
                    canonical_id.is_none_or(|id| id == selected)
                } else {
                    canonical_id == Some(selected)
                };
            if matches {
                scopes.push(counters);
            }
            let explicitly_unused = [
                "inputTokens",
                "outputTokens",
                "cacheCreationInputTokens",
                "cacheReadInputTokens",
            ]
            .iter()
            .all(|key| model.get(key).and_then(Value::as_u64) == Some(0));
            if !(matches || identity_valid && explicitly_unused) {
                // Detection follows the native invocation. Retain its charge even on refusal;
                // this check cannot prevent an internal client request from having occurred.
                refuse("Claude model usage reports an unexpected model identity");
            }
            let group = canonical_id.filter(|id| valid_id(id)).unwrap_or(key);
            if let Some(prior) = groups.get_mut(group) {
                // Aliased entries may overlap. Keep the larger observed total with unavailable
                // dimensions, rather than summing duplicate identities as independent spend.
                *prior = prior.lower_bound(counters);
                complete = false;
                refuse("Claude model usage contains ambiguous duplicate identities");
            } else {
                groups.insert(group, counters);
            }
        }
    } else {
        complete = false;
        refuse("Claude model usage is not an object");
    }
    let mut sum = Counters {
        complete: true,
        ..Counters::default()
    };
    for counters in groups.values() {
        // At most 32 model entries, each native component <= u64::MAX. Exact u128 sums
        // therefore remain below 32 * 2^64, and their three-component charge below 96 * 2^64.
        let add = |a: Option<u128>, b: Option<u128>| match (a, b) {
            (None, None) => None,
            _ => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        sum.input = add(sum.input, counters.input);
        sum.output = add(sum.output, counters.output);
        sum.write = add(sum.write, counters.write);
        sum.read = add(sum.read, counters.read);
        sum.charge_floor += counters.charge();
        sum.complete &= counters.complete;
    }
    let matching_scope = if top.same_bill(sum) {
        Some(sum)
    } else {
        scopes.into_iter().find(|scope| top.same_bill(*scope))
    };
    if matching_scope.is_none() {
        complete = false;
        refuse("Claude top-level and per-model usage cannot be reconciled");
    }
    if matching_scope.is_some_and(|scope| top.read_conflicts(scope)) {
        refuse("Claude top-level and per-model cache-read metadata disagree");
    }
    let model_usage = sum.usage();
    let charge =
        |usage: &Option<TaskTokenUsageV3>| usage.as_ref().map_or(0, |u| u.chargeable_tokens.get());
    let usage = if complete || charge(&model_usage) >= charge(&top_usage) {
        model_usage.or(top_usage)
    } else {
        top_usage
    };
    // Partial maps and conflicting summaries overlap: use the larger valid charge floor,
    // never top-level + map. Store retains the original reservation when reporting is incomplete.
    let observation = (!complete || error.is_some() || top_observation.is_some()).then(|| {
        TaskUsageObservationV1 {
            reported_usage: usage.clone(),
            charge_complete: complete,
        }
    });
    Accounting {
        usage,
        observation,
        error,
    }
}

#[cfg(test)]
mod tests;
