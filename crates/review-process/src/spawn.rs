//! macOS creates pipes and sets close-on-exec in separate system calls. Another
//! concurrent spawn can otherwise inherit a sibling's pipe before that flag is set.

use std::process::{Child, Command};

pub(crate) fn spawn(command: &mut Command) -> std::io::Result<Child> {
    // The pinned Rust standard library uses pipe + fcntl on Apple platforms.
    // Hold this only while creating the child, never during execution or draining.
    // Both buffered and duplex supervision must enter this same critical section.
    #[cfg(target_vendor = "apple")]
    let _creation = {
        static CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
        CREATION.lock().unwrap_or_else(|error| error.into_inner())
    };
    command.spawn()
}
