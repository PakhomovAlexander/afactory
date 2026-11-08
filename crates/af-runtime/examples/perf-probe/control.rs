//! The region handshake of a profiling build (the `perf-control` feature, ADR-0174, ADR-0175).
//!
//! A CPU profile of a library case must record its named region, not the fixture setup around
//! it, which can take longer than the whole capture. In a `perf-control` build whose driver names
//! three paths, [`READY_ENV`] (a FIFO this probe writes), [`GO_ENV`] (a FIFO this probe reads)
//! and [`REQUEST_ENV`] (the request file), every region is bracketed: `begin\n` before its clock
//! starts and `end\n` after it stops, each answered by `go\n` once the driver has enabled or
//! disabled the profiler and seen it acknowledge.
//!
//! A region far shorter than the profiler's sampling period is never sampled by a profiler that
//! starts afresh each time, yet every declared case runs in a fresh process. So a driven probe
//! delivers each repetition's message on the channel, `record N\n` and its N bytes, never on
//! stdout, and asks `next\n`; the driver judges the message, replaces the request's scratch and
//! seed copy with fresh ones and answers `again\n` or `done\n`. On `again` the probe executes
//! its own verified image afresh (`execve`) with the request file as stdin: nothing of the last
//! repetition survives in the process, while the kernel task and the profiler's event attached
//! to it continue, so the sampling period accumulates across short regions (ADR-0175). The
//! handshake is outside the timed region; without the feature, or without all three variables,
//! it does nothing at all and the probe runs once, writing its message to stdout. A handshake
//! that fails ends the probe: a capture whose region cannot be bracketed must not pass as one.

/// The FIFO this probe writes `begin`, `end` and `next` to.
#[cfg_attr(not(feature = "perf-control"), allow(dead_code))]
pub const READY_ENV: &str = "AF_PERF_REGION_READY";
/// The FIFO this probe reads each answer from.
#[cfg_attr(not(feature = "perf-control"), allow(dead_code))]
pub const GO_ENV: &str = "AF_PERF_REGION_GO";
/// The request file a fresh execution of a repetition reads as its stdin.
#[cfg_attr(not(feature = "perf-control"), allow(dead_code))]
pub const REQUEST_ENV: &str = "AF_PERF_REGION_REQUEST";

/// A bracketed region: its end is signalled when this guard drops.
#[must_use = "the region ends when this guard drops"]
pub struct Region {
    #[cfg(feature = "perf-control")]
    armed: bool,
}

/// Signal the start of a region and wait until the driver lets it run.
pub fn begin() -> Region {
    #[cfg(feature = "perf-control")]
    {
        let armed = handshake::exchange(b"begin\n", &[b"go\n"]).is_some();
        Region { armed }
    }
    #[cfg(not(feature = "perf-control"))]
    {
        Region {}
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        #[cfg(feature = "perf-control")]
        if self.armed {
            handshake::exchange(b"end\n", &[b"go\n"]);
        }
    }
}

/// Whether a driver takes this probe's messages on the channel (always false without one).
#[allow(dead_code)]
pub fn driven() -> bool {
    #[cfg(feature = "perf-control")]
    {
        handshake::driven()
    }
    #[cfg(not(feature = "perf-control"))]
    {
        false
    }
}

/// Deliver one repetition's message to the driver, `record N\n` and its bytes; nothing
/// without a driver. (The integration test includes this module without the probe's `main`,
/// which alone delivers.)
#[allow(dead_code)]
pub fn deliver(message: &[u8]) {
    #[cfg(feature = "perf-control")]
    {
        let mut framed = format!("record {}\n", message.len()).into_bytes();
        framed.extend_from_slice(message);
        handshake::send(&framed);
    }
    #[cfg(not(feature = "perf-control"))]
    {
        let _ = message;
    }
}

/// Ask the driver for another repetition of the same request, its fresh state already in
/// place. On `again` this never returns: the probe executes its own image afresh with the
/// request file as stdin. It returns at once without a driver, and on `done`.
#[allow(dead_code)]
pub fn another() {
    #[cfg(feature = "perf-control")]
    if handshake::exchange(b"next\n", &[b"again\n", b"done\n"]) == Some(0) {
        handshake::execute_afresh();
    }
}

#[cfg(feature = "perf-control")]
mod handshake {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::sync::{Mutex, OnceLock};

    struct Channel {
        ready: File,
        go: Option<File>,
    }

    pub fn driven() -> bool {
        [super::READY_ENV, super::GO_ENV, super::REQUEST_ENV]
            .iter()
            .all(|name| std::env::var_os(name).is_some())
    }

    fn channel() -> Option<&'static Mutex<Channel>> {
        static CHANNEL: OnceLock<Option<Mutex<Channel>>> = OnceLock::new();
        CHANNEL
            .get_or_init(|| {
                if !driven() {
                    return None;
                }
                let ready = std::env::var_os(super::READY_ENV)?;
                let ready = std::fs::OpenOptions::new()
                    .write(true)
                    .open(ready)
                    .unwrap_or_else(|error| fail(&format!("cannot open the ready FIFO: {error}")));
                Some(Mutex::new(Channel { ready, go: None }))
            })
            .as_ref()
    }

    /// Send `message` without waiting for an answer; nothing without a driver.
    pub fn send(message: &[u8]) {
        let Some(channel) = channel() else {
            return;
        };
        let mut channel = channel
            .lock()
            .unwrap_or_else(|_| fail("the handshake lock is poisoned"));
        channel
            .ready
            .write_all(message)
            .and_then(|()| channel.ready.flush())
            .unwrap_or_else(|error| fail(&format!("cannot signal the driver: {error}")));
    }

    /// Execute this probe's own image afresh for the next repetition, the request file as its
    /// stdin; it never returns. Every descriptor this process opened closes on exec; stdout and
    /// stderr stay the driver's.
    pub fn execute_afresh() -> ! {
        use std::os::unix::process::CommandExt;
        let request = std::env::var_os(super::REQUEST_ENV)
            .unwrap_or_else(|| fail("the request file is no longer named"));
        let request = File::open(request)
            .unwrap_or_else(|error| fail(&format!("cannot open the request file: {error}")));
        // The very image this process runs where the system names it, so a path replaced since
        // the launch is never executed; otherwise the path it was started from.
        let own = std::path::Path::new("/proc/self/exe");
        let image = if own.exists() {
            own.to_path_buf()
        } else {
            std::env::current_exe()
                .unwrap_or_else(|error| fail(&format!("cannot name this probe's image: {error}")))
        };
        let error = std::process::Command::new(image).stdin(request).exec();
        fail(&format!("cannot execute a fresh repetition: {error}"))
    }

    /// Send `message` and wait for one of `answers`; its index, or None without a driver.
    pub fn exchange(message: &[u8], answers: &[&[u8]]) -> Option<usize> {
        let channel = channel()?;
        let mut channel = channel
            .lock()
            .unwrap_or_else(|_| fail("the handshake lock is poisoned"));
        channel
            .ready
            .write_all(message)
            .and_then(|()| channel.ready.flush())
            .unwrap_or_else(|error| fail(&format!("cannot signal the driver: {error}")));
        if channel.go.is_none() {
            // Opened after the first message, so the driver knows a reader is waiting.
            let path = std::env::var_os(super::GO_ENV)
                .unwrap_or_else(|| fail("the go FIFO is no longer named"));
            channel.go = Some(
                File::open(path)
                    .unwrap_or_else(|error| fail(&format!("cannot open the go FIFO: {error}"))),
            );
        }
        let go = channel.go.as_mut().expect("opened above");
        // One line of at most 16 bytes, read a byte at a time so nothing after it is consumed.
        let mut answer = Vec::new();
        while !answer.ends_with(b"\n") {
            let mut byte = [0_u8; 1];
            go.read_exact(&mut byte)
                .unwrap_or_else(|error| fail(&format!("the driver did not answer: {error}")));
            answer.push(byte[0]);
            if answer.len() > 16 {
                fail("the driver's answer is longer than one short line");
            }
        }
        answers
            .iter()
            .position(|expected| *expected == answer.as_slice())
            .or_else(|| fail("the driver answered something it may not"))
    }

    fn fail(detail: &str) -> ! {
        eprintln!("perf-probe: region handshake: {detail}");
        std::process::exit(70);
    }
}
