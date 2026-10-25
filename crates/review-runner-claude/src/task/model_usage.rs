//! Reconcile overlapping native summaries; never add top-level usage to its model breakdown
//! (ADR-0102, ADR-0125).
use crate::hint::{BoundModel, ModelOutcome, OtherModel, compose};
use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use review_runner::task::usage::NativeCounter;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_MODELS: usize = 32;
const MAX_MODEL_ID_BYTES: usize = 256;
/// The four counters of one `modelUsage` entry, in [`Counters::parse`] order.
const MODEL_COUNTERS: [&str; 4] = [
    "inputTokens",
    "outputTokens",
    "cacheCreationInputTokens",
    "cacheReadInputTokens",
];

pub(super) struct Accounting {
    pub usage: Option<TaskTokenUsageV3>,
    pub observation: Option<TaskUsageObservationV1>,
    pub error: Option<String>,
    /// The error starts with the model-switch refusal, worded by [`crate::hint::compose`]
    /// (ADR-0147).
    pub switched: bool,
}

/// Record why the reply is refused, once per reason, in the order found. Without a model switch
/// the first reason is the one reported; a switch outranks them all and they follow its hint.
fn refuse(reasons: &mut Vec<&'static str>, reason: &'static str) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
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

    /// At least one counter is above zero.
    fn active(self) -> bool {
        [self.input, self.output, self.write, self.read]
            .iter()
            .any(|counter| counter.is_some_and(|n| n > 0))
    }

    fn same_bill(self, other: Self) -> bool {
        self.complete
            && other.complete
            && (self.input, self.output, self.write) == (other.input, other.output, other.write)
    }

    /// No billed component of `self` exceeds `other`'s: `other` can describe the same requests
    /// plus more, never fewer.
    fn within(self, other: Self) -> bool {
        self.complete
            && other.complete
            && self.input <= other.input
            && self.output <= other.output
            && self.write <= other.write
    }

    fn read_conflicts(self, other: Self) -> bool {
        matches!((self.read, other.read), (Some(a), Some(b)) if a != b)
    }

    fn read_exceeds(self, other: Self) -> bool {
        matches!((self.read, other.read), (Some(a), Some(b)) if a > b)
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

/// Whether the entry `key` is well formed and names the bound model `selected`: the bound ID
/// itself, or an alias whose `canonicalModel` is it. Returns (well formed, bound).
fn identity(key: &str, model: &Value, selected: &str) -> (bool, bool) {
    let canonical = model.get("canonicalModel");
    let canonical_id = canonical.and_then(Value::as_str);
    let valid = valid_id(key)
        && canonical.is_none_or(|_| canonical_id.is_some_and(valid_id))
        && !(key == selected && canonical_id.is_some_and(|id| id != selected));
    let bound = valid
        && if key == selected {
            canonical_id.is_none_or(|id| id == selected)
        } else {
            canonical_id == Some(selected)
        };
    (valid, bound)
}

/// FOREIGN ACTIVITY (ADR-0147): an entry whose key is a well-formed ID that is neither the bound
/// model nor an alias whose `canonicalModel` is the bound model, whose four counters parse
/// completely and at least one of which is above zero. A malformed or all-zero foreign entry is
/// never one; a key outside grammar G is named `[not printable]` by the hint.
fn foreign_activity(key: &str, model: &Value, selected: &str) -> Option<OtherModel> {
    if !valid_id(key)
        || key == selected
        || model.get("canonicalModel").and_then(Value::as_str) == Some(selected)
    {
        return None;
    }
    let counters = Counters::parse(model, MODEL_COUNTERS);
    (!counters.malformed && counters.active()).then(|| OtherModel {
        model: key.into(),
        output_tokens: model.get("outputTokens").and_then(Value::as_u64),
    })
}

/// The bound on the reset time a usage-limit reply's words may name, in bytes.
const MAX_RESET_BYTES: usize = 48;

/// What a Claude reply that is the account's usage-limit refusal says, in af's words: the limit
/// is reached, and when it resets as the reply states it (an epoch after `|`, or the time after
/// `resets`, kept only from a closed alphabet), or that the reply does not say. `None` for any
/// other reply. Such a reply answers with an empty `modelUsage`, which is no malformed map: the
/// model never ran. The words name a rate limit, so the Attempt's cause is `provider_capacity`.
fn usage_limit(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if value.get("is_error").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let text = value.get("result").and_then(Value::as_str)?;
    let lower = text.to_ascii_lowercase();
    if !["usage limit", "hit your limit", "limit reached"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return None;
    }
    let epoch = text
        .rsplit_once('|')
        .map(|(_, epoch)| epoch.trim())
        .and_then(|epoch| epoch.parse::<u64>().ok())
        .filter(|epoch| *epoch > 0 && *epoch < 1 << 40);
    let said = lower.find("resets ").map(|at| {
        text[at + "resets ".len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || " :()/_+-".contains(*c))
            .collect::<String>()
            .trim()
            .to_string()
    });
    let reset = match (epoch, said) {
        (Some(epoch), _) => Some(review_core::task::collection::collected_time(
            epoch.saturating_mul(1_000),
        )),
        (None, Some(said)) if !said.is_empty() && said.len() <= MAX_RESET_BYTES => Some(said),
        _ => None,
    };
    Some(match reset {
        Some(reset) => {
            format!(
                "the Claude account's usage limit is reached; it resets at {reset} (rate limit)"
            )
        }
        None => "the Claude account's usage limit is reached; reset time not reported (rate limit)"
            .to_string(),
    })
}

/// Account for one result, `selected` being the bound `--model` and `effort` its `--effort`.
pub(super) fn account(value: Option<&Value>, selected: &str, effort: Option<&str>) -> Accounting {
    let (top_usage, top_observation) = super::parse_usage(value);
    let Some(map) = value.and_then(|v| v.get("modelUsage")) else {
        // Preserve the top-level-only native contract, including its malformed-usage observation.
        return Accounting {
            usage: top_usage,
            observation: top_observation,
            error: None,
            switched: false,
        };
    };
    // The account's usage limit: no model ran, so the empty map is the reply's, not a malformed
    // one, and the refusal says what happened rather than the map's bound. Its charge stays
    // incomplete, so the Store keeps the reservation.
    if map.as_object().is_some_and(serde_json::Map::is_empty)
        && let Some(limit) = usage_limit(value)
    {
        return Accounting {
            observation: Some(TaskUsageObservationV1 {
                reported_usage: top_usage.clone(),
                charge_complete: false,
            }),
            usage: top_usage,
            error: Some(limit),
            switched: false,
        };
    }
    let mut reasons = Vec::new();
    // The switch decision reads every entry of the map, however many: the charge below reads at
    // most 32 (ADR-0102), but a foreign activity anywhere is a switch, and the switch refusal
    // then leads, whatever else was found. It names the bound model's own output and every
    // foreign activity, with its output, in map order.
    let mut bound_output_tokens = None;
    let mut others = Vec::new();
    if let Some(entries) = map.as_object() {
        for (key, model) in entries {
            if bound_output_tokens.is_none() && identity(key, model, selected).1 {
                bound_output_tokens = model.get("outputTokens").and_then(Value::as_u64);
            }
            others.extend(foreign_activity(key, model, selected));
        }
    }
    let switched = !others.is_empty();
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
        refuse(&mut reasons, "Claude top-level usage is malformed");
    }
    let mut groups = BTreeMap::<&str, Counters>::new();
    let mut scopes = Vec::new();
    if let Some(entries) = map.as_object() {
        if entries.is_empty() || entries.len() > MAX_MODELS {
            complete = false;
            refuse(
                &mut reasons,
                "Claude model usage exceeds its finite nonempty model bound",
            );
        }
        for (key, model) in entries.iter().take(MAX_MODELS) {
            let counters = Counters::parse(model, MODEL_COUNTERS);
            complete &= counters.complete;
            if counters.malformed {
                refuse(&mut reasons, "Claude per-model usage is malformed");
            }
            let (identity_valid, matches) = identity(key, model, selected);
            if !identity_valid {
                complete = false;
                refuse(&mut reasons, "Claude model usage identity is malformed");
            }
            if matches {
                scopes.push(counters);
            }
            let explicitly_unused = MODEL_COUNTERS
                .iter()
                .all(|key| model.get(key).and_then(Value::as_u64) == Some(0));
            // A foreign activity was found above, and its charge is retained even on refusal:
            // this check cannot prevent an internal client request from having occurred. A
            // well-formed foreign entry with complete counters, none above zero, that leaves one
            // out is neither a switch nor explicitly unused, and is still refused.
            if identity_valid
                && !matches
                && !counters.malformed
                && !counters.active()
                && !explicitly_unused
            {
                refuse(
                    &mut reasons,
                    "Claude model usage leaves another model's activity unreported",
                );
            }
            let canonical_id = model.get("canonicalModel").and_then(Value::as_str);
            let group = canonical_id.filter(|id| valid_id(id)).unwrap_or(key);
            if let Some(prior) = groups.get_mut(group) {
                // Aliased entries may overlap. Keep the larger observed total with unavailable
                // dimensions, rather than summing duplicate identities as independent spend.
                *prior = prior.lower_bound(counters);
                complete = false;
                refuse(
                    &mut reasons,
                    "Claude model usage contains ambiguous duplicate identities",
                );
            } else {
                groups.insert(group, counters);
            }
        }
    } else {
        complete = false;
        refuse(&mut reasons, "Claude model usage is not an object");
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
    // Native 2.1.285 counts requests in the breakdown that its top-level summary leaves out.
    // A breakdown no top-level component exceeds is then the whole bill. A top-level component
    // above the breakdown is spend no reported model explains, and stays unreconciled.
    let covered = matching_scope.is_none() && top.within(sum);
    if matching_scope.is_none() && !covered {
        complete = false;
        refuse(
            &mut reasons,
            "Claude top-level and per-model usage cannot be reconciled",
        );
    }
    if matching_scope.is_some_and(|scope| top.read_conflicts(scope))
        || covered && top.read_exceeds(sum)
    {
        refuse(
            &mut reasons,
            "Claude top-level and per-model cache-read metadata disagree",
        );
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
    let refused = switched || !reasons.is_empty();
    let observation =
        (!complete || refused || top_observation.is_some()).then(|| TaskUsageObservationV1 {
            reported_usage: usage.clone(),
            charge_complete: complete,
        });
    let count = |key| value.and_then(|v| v.get(key)).and_then(Value::as_u64);
    let error = if switched {
        // The switch refusal outranks every other reason; each one found follows it, so no
        // accounting fact is lost and the diagnostic still starts with the hint.
        let mut error = compose(
            BoundModel {
                model: selected,
                effort,
                provider: None,
            },
            &ModelOutcome::Switched {
                bound_output_tokens,
                others: &others,
                safety_stops: count("safety_stops"),
                num_turns: count("num_turns"),
            },
        );
        for reason in &reasons {
            error.push_str("; ");
            error.push_str(reason);
        }
        Some(error)
    } else {
        reasons.first().map(|reason| (*reason).to_owned())
    };
    Accounting {
        usage,
        observation,
        error,
        switched,
    }
}

#[cfg(test)]
mod tests;
