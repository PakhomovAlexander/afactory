//! The policy of the in-Attempt retry after a transient Provider failure (ADR-0208): a network
//! failure, a model at capacity, a timeout, or a native exit without output. The Provider's own
//! `Retry-After` is honoured; otherwise the first wait is 60 s and each later one doubles. An
//! Attempt retries at most [`MAX_PROVIDER_RETRIES`] times and waits at most ten minutes in all,
//! whichever comes first, and only while its wall and its reservation leave room: a wait never
//! extends a deadline or resets a budget. A failure that outlasts the retries fails the node with
//! its class and the retry history in its diagnostic.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use review_core::task::provider_event::{MAX_PROVIDER_RETRIES, ProviderEventV1};
use review_runner::native_failure::{Transient, TransientClass};
use review_store::store::task::execution::PreparedTaskAttempt;

use super::TaskWorkOutput;

/// The first wait when the Provider names none.
const BASE_WAIT: Duration = Duration::from_secs(60);
/// The most an Attempt waits in all.
const MAX_TOTAL_WAIT: Duration = Duration::from_secs(600);

/// The first wait, which a debug build's test may shorten with `AF_TEST_PROVIDER_RETRY_BASE_MS`
/// (1 ms to 60 s) so that a test of the retry path does not wait a minute. A release build always
/// waits the production base.
fn base_wait() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("AF_TEST_PROVIDER_RETRY_BASE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|ms| (1..=60_000).contains(ms))
    {
        return Duration::from_millis(ms);
    }
    BASE_WAIT
}

/// One Attempt's retries so far and why it stopped retrying.
#[derive(Default)]
pub(super) struct Retries {
    history: Vec<(TransientClass, Duration)>,
    waited: Duration,
    stopped: Option<String>,
}

impl Retries {
    /// The wait before the next call after `transient`, or why there is none.
    pub(super) fn next_wait(
        &self,
        transient: Transient,
        result: &TaskWorkOutput,
        attempt: &PreparedTaskAttempt,
    ) -> Result<Duration, String> {
        let retry = self.history.len() as u32 + 1;
        if retry > MAX_PROVIDER_RETRIES {
            return Err(format!("its {MAX_PROVIDER_RETRIES} retries are spent"));
        }
        if result.refused_input {
            return Err("the Provider refused this input".into());
        }
        if result.usage_observation.is_some() {
            return Err("its usage was not completely reported".into());
        }
        let wait = transient
            .retry_after
            .unwrap_or_else(|| base_wait().saturating_mul(1 << (retry - 1)));
        if self.waited + wait > MAX_TOTAL_WAIT {
            return Err("its ten minutes of waiting are spent".into());
        }
        let charged = result
            .charged_tokens
            .into_iter()
            .chain(result.usage.as_ref().map(|u| u.chargeable_tokens.get()))
            .max()
            .unwrap_or(0);
        if charged >= u128::from(attempt.reservation().tokens) {
            return Err("its token reservation is spent".into());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64);
        let left = attempt.reservation().deadline_unix_ms.saturating_sub(now);
        if Duration::from_millis(left) <= wait {
            return Err("its wall would be spent before the next call".into());
        }
        Ok(wait)
    }

    /// The event recorded before the wait for the next call.
    pub(super) fn event(&self, transient: Transient, wait: Duration) -> ProviderEventV1 {
        ProviderEventV1::RetryWait {
            class: transient.class,
            retry: self.history.len() as u32 + 1,
            max_retries: MAX_PROVIDER_RETRIES,
            wait_ms: (wait.as_millis() as u64).max(1),
            retry_after: transient.retry_after.is_some(),
        }
    }

    pub(super) fn waited(&mut self, transient: Transient, wait: Duration) {
        self.history.push((transient.class, wait));
        self.waited += wait;
    }

    pub(super) fn stopped(&mut self, why: String) {
        self.stopped = Some(why);
    }

    /// A failure after retries carries its class and the retry history in its diagnostic.
    pub(super) fn annotate(&self, result: &mut TaskWorkOutput) {
        let Err(error) = &mut result.outputs else {
            return;
        };
        let class = result.notes.transient.map(|transient| transient.class);
        if self.history.is_empty() && class.is_none() {
            return;
        }
        let history = self
            .history
            .iter()
            .enumerate()
            .map(|(index, (class, wait))| {
                format!(
                    "retry {} after {} waited {}",
                    index + 1,
                    class.as_str(),
                    seconds(*wait)
                )
            })
            .collect::<Vec<_>>();
        let mut note = match class {
            Some(class) => format!(
                "; transient Provider failure ({}) after {} of {MAX_PROVIDER_RETRIES} retries",
                class.as_str(),
                self.history.len()
            ),
            None => match self.history.len() {
                1 => "; after 1 retry".to_string(),
                retries => format!("; after {retries} retries"),
            },
        };
        if !history.is_empty() {
            note.push_str(&format!(" ({})", history.join(", ")));
        }
        if let Some(why) = &self.stopped {
            note.push_str(&format!("; not retried again: {why}"));
        }
        error.push_str(&note);
    }
}

fn seconds(wait: Duration) -> String {
    if wait.as_millis() % 1000 == 0 {
        format!("{} s", wait.as_secs())
    } else {
        format!("{} ms", wait.as_millis())
    }
}

/// Wait `wait`, or until `cancellation` is set; whether the whole wait passed.
pub(super) fn sleep(wait: Duration, cancellation: Option<&AtomicBool>) -> bool {
    let until = Instant::now() + wait;
    loop {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return false;
        }
        let Some(left) = until.checked_duration_since(Instant::now()) else {
            return true;
        };
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
}

/// Two calls of one Attempt as one result: their reported usage adds, their raw evidence is
/// kept — the later call's first, so the first raw artifact stays the reply the Attempt's output
/// came from — and the later call's outcome is the Attempt's. A failed call that reported no
/// usage adds nothing (ADR-0143); only when no call reported any is the Attempt's usage unknown.
pub(super) fn merge(earlier: TaskWorkOutput, later: TaskWorkOutput) -> TaskWorkOutput {
    let mut raw = later.raw_artifact_ids;
    for id in earlier.raw_artifact_ids {
        if !raw.contains(&id) {
            raw.push(id);
        }
    }
    let usage = review_runner::task::merge_usage(
        earlier.usage,
        later
            .usage
            .or_else(|| later.usage_observation.as_ref()?.reported_usage.clone()),
    );
    let charged_tokens = match (earlier.charged_tokens, later.charged_tokens) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    }
    .into_iter()
    .chain(usage.as_ref().map(|usage| usage.chargeable_tokens.get()))
    .max();
    let usage_observation = later.usage_observation.map(|observation| {
        review_core::task::usage::TaskUsageObservationV1 {
            reported_usage: usage.clone(),
            charge_complete: observation.charge_complete,
        }
    });
    let mut notes = earlier.notes;
    notes = notes.then(later.notes);
    TaskWorkOutput {
        notes,
        usage_observation,
        usage,
        outputs: later.outputs,
        charged_tokens,
        raw_artifact_ids: raw,
        usage_id: later.usage_id.or(earlier.usage_id),
        feedback_id: later.feedback_id,
        unknown_usage_cause: later.unknown_usage_cause.or(earlier.unknown_usage_cause),
        refused_input: later.refused_input,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::usage::TaskTokenUsageV3;

    fn output(charged: Option<u128>, failed: bool) -> TaskWorkOutput {
        TaskWorkOutput {
            notes: Default::default(),
            usage_observation: None,
            usage: charged.map(TaskTokenUsageV3::charge_only),
            outputs: if failed {
                Err("Provider model at capacity (capacity)".into())
            } else {
                Ok(Default::default())
            },
            charged_tokens: charged,
            raw_artifact_ids: vec![format!("sha256:{}", "a".repeat(64))],
            usage_id: None,
            feedback_id: None,
            unknown_usage_cause: None,
            refused_input: false,
        }
    }

    #[test]
    fn calls_of_one_attempt_add_their_usage_and_keep_the_later_outcome() {
        let merged = merge(output(Some(7), true), output(Some(5), false));
        assert_eq!(merged.charged_tokens, Some(12));
        assert_eq!(merged.usage.unwrap().chargeable_tokens.get(), 12);
        assert!(merged.outputs.is_ok());
        assert_eq!(merged.raw_artifact_ids.len(), 1, "one raw blob, kept once");
        let unknown_first = merge(output(None, true), output(Some(5), false));
        assert_eq!(unknown_first.charged_tokens, Some(5));
        let unknown_both = merge(output(None, true), output(None, true));
        assert_eq!(unknown_both.charged_tokens, None);
    }

    #[test]
    fn the_history_names_each_wait_and_the_class_that_outlasted_them() {
        let mut retries = Retries::default();
        let capacity = Transient::new(TransientClass::Capacity);
        retries.waited(capacity, Duration::from_secs(60));
        retries.waited(capacity, Duration::from_secs(120));
        assert_eq!(
            retries.event(capacity, Duration::from_secs(240)),
            ProviderEventV1::RetryWait {
                class: TransientClass::Capacity,
                retry: 3,
                max_retries: 3,
                wait_ms: 240_000,
                retry_after: false,
            },
            "the event names the retry it waits for"
        );
        retries.waited(
            Transient::new(TransientClass::Network),
            Duration::from_secs(240),
        );
        retries.stopped(format!("its {MAX_PROVIDER_RETRIES} retries are spent"));
        let mut result = output(None, true);
        result.notes.transient = Some(capacity);
        retries.annotate(&mut result);
        assert_eq!(
            result.outputs.unwrap_err(),
            "Provider model at capacity (capacity); transient Provider failure (capacity) after \
             3 of 3 retries (retry 1 after capacity waited 60 s, retry 2 after capacity waited \
             120 s, retry 3 after network waited 240 s); not retried again: its 3 retries are \
             spent"
        );
    }

    #[test]
    fn a_cancelled_wait_ends_early() {
        let flag = AtomicBool::new(true);
        let started = Instant::now();
        assert!(!sleep(Duration::from_secs(30), Some(&flag)));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(sleep(Duration::from_millis(10), None));
    }
}
