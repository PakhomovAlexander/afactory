//! Cooperative cancellation for bounded channel waits; absent control keeps the old wait.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

pub(crate) fn cancelled(flag: Option<&AtomicBool>) -> bool {
    flag.is_some_and(|flag| flag.load(Ordering::Acquire))
}

pub(crate) enum ReceiveError {
    Cancelled,
    Timeout,
    Disconnected,
}

pub(crate) fn receive<T>(
    receiver: &mpsc::Receiver<T>,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
) -> Result<T, ReceiveError> {
    let Some(flag) = cancellation else {
        return receiver.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => ReceiveError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => ReceiveError::Disconnected,
        });
    };
    let deadline = Instant::now() + timeout;
    loop {
        if flag.load(Ordering::Acquire) {
            return Err(ReceiveError::Cancelled);
        }
        match receiver.recv_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20)),
        ) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(ReceiveError::Disconnected),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                return Err(ReceiveError::Timeout);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}
