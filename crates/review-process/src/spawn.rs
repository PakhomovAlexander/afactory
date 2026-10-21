//! macOS creates pipes and sets close-on-exec in separate system calls. Another
//! concurrent spawn can otherwise inherit a sibling's pipe before that flag is set.

//!
//! Linux refuses to execute a file that another concurrently forked child still holds open
//! for writing (`ETXTBSY`): the writer has closed it, but the child inherited the descriptor
//! and has not reached `exec` yet. The window is a few milliseconds; a bounded retry covers it.

use std::process::{Child, Command};
use std::time::Duration;

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
