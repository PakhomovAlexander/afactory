//! macOS creates pipes and sets close-on-exec in separate system calls. Another
//! concurrent spawn can otherwise inherit a sibling's pipe before that flag is set.

//!
//! Linux refuses to execute a file that another concurrently forked child still holds open
//! for writing (`ETXTBSY`): the writer has closed it, but the child inherited the descriptor
//! and has not reached `exec` yet. The window is a few milliseconds; a bounded retry covers it.

use std::process::{Child, Command};
use std::time::Duration;

/// Execution authority checked immediately before each OS spawn attempt. Separate from
/// supervision of an admitted process (for example, settling daemon creation).
#[derive(Clone, Copy)]
pub struct SpawnAdmission<'a> {
    pub deadline: std::time::Instant,
    pub cancellation: Option<&'a std::sync::atomic::AtomicBool>,
}

const BUSY_RETRIES: u32 = 100;
const BUSY_RETRY_DELAY: Duration = Duration::from_millis(10);

/// The critical section in which a supervised child's pipes are created and the child spawned.
/// The pinned Rust standard library uses pipe + fcntl on Apple platforms, and so does
/// `sample`'s own spawn. Hold this only while creating the child, never during execution or
/// draining; every spawn of this crate enters this same critical section.
pub(crate) fn creation() -> Option<std::sync::MutexGuard<'static, ()>> {
    #[cfg(target_vendor = "apple")]
    {
        static CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
        Some(CREATION.lock().unwrap_or_else(|error| error.into_inner()))
    }
    #[cfg(not(target_vendor = "apple"))]
    None
}

/// Spawn `command` through the one boundary every supervised process shares.
pub fn spawn(command: &mut Command) -> std::io::Result<Child> {
    let _creation = creation();
    let mut busy_retries = 0;
    loop {
        match command.spawn() {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && busy_retries < BUSY_RETRIES =>
            {
                busy_retries += 1;
                std::thread::sleep(BUSY_RETRY_DELAY);
            }
            result => return result,
        }
    }
}

/// Admit a launch only while the caller's absolute allowance and cancellation permit it.
/// The final check precedes the OS spawn call; it is not a daemon-side atomic deadline.
pub(crate) fn spawn_until(
    command: &mut Command,
    deadline: std::time::Instant,
    signals: &[&std::sync::atomic::AtomicBool],
) -> Result<Child, crate::SupervisedError> {
    spawn_at_admission(command, deadline, signals, || {
        #[cfg(test)]
        ADMISSION_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook();
            }
        });
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static ADMISSION_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        std::cell::RefCell::new(None);
}

fn spawn_at_admission(
    command: &mut Command,
    deadline: std::time::Instant,
    signals: &[&std::sync::atomic::AtomicBool],
    before_admission: impl FnMut(),
) -> Result<Child, crate::SupervisedError> {
    spawn_attempts_until(command, deadline, signals, before_admission, Command::spawn)
}

fn spawn_attempts_until(
    command: &mut Command,
    deadline: std::time::Instant,
    signals: &[&std::sync::atomic::AtomicBool],
    mut before_admission: impl FnMut(),
    mut attempt: impl FnMut(&mut Command) -> std::io::Result<Child>,
) -> Result<Child, crate::SupervisedError> {
    let _creation = creation();
    let mut busy_retries = 0;
    loop {
        before_admission();
        if signals
            .iter()
            .any(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(crate::SupervisedError::Cancelled);
        }
        if std::time::Instant::now() >= deadline {
            return Err(crate::SupervisedError::TimedOut {
                stdout: vec![],
                stderr: vec![],
                pid: None,
                rusage: None,
            });
        }
        match attempt(command) {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && busy_retries < BUSY_RETRIES =>
            {
                busy_retries += 1;
                std::thread::sleep(
                    BUSY_RETRY_DELAY
                        .min(deadline.saturating_duration_since(std::time::Instant::now())),
                );
            }
            result => return result.map_err(crate::SupervisedError::Spawn),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_retry_rechecks_execution_authority_before_another_os_attempt() {
        for cancel in [false, true] {
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            let flag = std::sync::atomic::AtomicBool::new(false);
            let attempts = std::cell::Cell::new(0);
            let result = spawn_attempts_until(
                &mut Command::new("/path/that/must/never/be/spawned"),
                deadline,
                &[&flag],
                || {
                    if attempts.get() == 1 {
                        if cancel {
                            flag.store(true, std::sync::atomic::Ordering::Release);
                        } else {
                            let (_send, receive) = std::sync::mpsc::channel::<()>();
                            assert!(matches!(
                                receive.recv_timeout(
                                    deadline.saturating_duration_since(std::time::Instant::now())
                                ),
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                            ));
                        }
                    }
                },
                |_| {
                    attempts.set(attempts.get() + 1);
                    assert_eq!(
                        attempts.get(),
                        1,
                        "no OS spawn after execution authority ends"
                    );
                    Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
                },
            );
            assert_eq!(attempts.get(), 1);
            assert!(if cancel {
                matches!(result, Err(crate::SupervisedError::Cancelled))
            } else {
                matches!(
                    result,
                    Err(crate::SupervisedError::TimedOut { pid: None, .. })
                )
            });
        }
    }

    #[test]
    fn expiry_and_cancellation_at_spawn_admission_never_launch_a_process() {
        for cancel in [false, true] {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let flag = std::sync::atomic::AtomicBool::new(false);
            let result = spawn_at_admission(
                &mut Command::new("/path/that/must/never/be/spawned"),
                deadline,
                &[&flag],
                || {
                    if cancel {
                        flag.store(true, std::sync::atomic::Ordering::Release);
                    } else {
                        let (_send, receive) = std::sync::mpsc::channel::<()>();
                        assert!(matches!(
                            receive.recv_timeout(
                                deadline.saturating_duration_since(std::time::Instant::now())
                            ),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        ));
                    }
                },
            );
            if cancel {
                assert!(matches!(result, Err(crate::SupervisedError::Cancelled)));
            } else {
                assert!(matches!(
                    result,
                    Err(crate::SupervisedError::TimedOut {
                        pid: None,
                        rusage: None,
                        ..
                    })
                ));
            }
        }
    }
}
