//! Exactly one Reviewer adapter invocation. Scheduling, reservation, retry, settlement and
//! domain publication belong to the caller; this operation cannot create another Attempt.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use review_broker::BrokerClient;
use review_runner::{ReceiptedReviewerReturn, ReviewerAdapter, ReviewerInputs, RunnerError};
use review_store::Cas;

pub(super) enum InvocationFailure {
    Adapter(RunnerError),
    Panicked,
}

pub(super) struct InvocationObservation {
    pub result: Result<ReceiptedReviewerReturn, InvocationFailure>,
    pub started: SystemTime,
    pub elapsed: Duration,
}

pub(super) fn invoke(
    cas: &Cas,
    adapter: &dyn ReviewerAdapter,
    sandbox: &Path,
    inputs: &ReviewerInputs,
    broker: Option<&dyn BrokerClient>,
) -> InvocationObservation {
    let started = SystemTime::now();
    let clock = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adapter.invoke_with_broker(cas, sandbox, inputs, broker)
    }))
    .map_err(|_| InvocationFailure::Panicked)
    .and_then(|result| result.map_err(InvocationFailure::Adapter));
    InvocationObservation {
        result,
        started,
        elapsed: clock.elapsed(),
    }
}
