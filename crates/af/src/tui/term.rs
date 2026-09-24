//! The terminal session: `/dev/tty` in raw mode on the alternate screen, restored on every exit
//! path — a normal return, an error, a hand-off to `$EDITOR`, and a panic.
//!
//! Raw mode keeps `VMIN = 0, VTIME = 1`, so a read returns within a tenth of a second whether
//! or not a key arrived. That is the event loop's only clock: background work is polled
//! between reads, and a key is never waited on longer than that.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::sync::{Mutex, MutexGuard, Once, PoisonError, TryLockError};
use std::thread::ThreadId;

use rustix::termios::{self, OptionalActions, SpecialCodeIndex, Termios};

use super::Host;
use super::paint::{self, Frame};

/// Alternate screen, cursor hidden, no autowrap, cleared.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[2J";
/// Plain paint, autowrap and cursor back, main screen back.
const LEAVE: &[u8] = b"\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l";

/// The modes to restore while a session holds the terminal, and the thread that holds it, for
/// the panic hook: a background thread's panic must not take the terminal from the event loop.
static SAVED: Mutex<Option<(Termios, ThreadId)>> = Mutex::new(None);

pub(crate) struct Session {
    tty: File,
    saved: Termios,
    active: bool,
    /// The screen was lost (a hand-off) and must be painted whole.
    dirty: bool,
}

impl Session {
    pub(crate) fn open() -> Result<Session, String> {
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|error| format!("opening the terminal: {error}"))?;
        let saved =
            termios::tcgetattr(&tty).map_err(|error| format!("reading terminal modes: {error}"))?;
        install_panic_hook();
        let mut session = Session {
            tty,
            saved,
            active: false,
            dirty: true,
        };
        session.enter()?;
        Ok(session)
    }

    fn enter(&mut self) -> Result<(), String> {
        let mut raw = self.saved.clone();
        raw.make_raw();
        raw.special_codes[SpecialCodeIndex::VMIN] = 0;
        raw.special_codes[SpecialCodeIndex::VTIME] = 1;
        *lock() = Some((self.saved.clone(), std::thread::current().id()));
        termios::tcsetattr(&self.tty, OptionalActions::Now, &raw)
            .map_err(|error| format!("entering raw mode: {error}"))?;
        self.active = true;
        self.dirty = true;
        self.send(ENTER)
    }

    fn leave(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let _ = self.tty.write_all(LEAVE);
        let _ = self.tty.flush();
        let _ = termios::tcsetattr(&self.tty, OptionalActions::Now, &self.saved);
        *lock() = None;
    }

    pub(crate) fn close(mut self) {
        self.leave();
    }

    /// Columns and rows; 80x24 when the terminal will not say.
    pub(crate) fn size(&self) -> (usize, usize) {
        match termios::tcgetwinsize(&self.tty) {
            Ok(size) if size.ws_col > 0 && size.ws_row > 0 => {
                (usize::from(size.ws_col), usize::from(size.ws_row))
            }
            _ => (80, 24),
        }
    }

    /// Whether the screen must be painted whole, once.
    pub(crate) fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// At most a tenth of a second of waiting for input.
    pub(crate) fn read(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        match self.tty.read(buffer) {
            Ok(count) => Ok(count),
            Err(error) if transient(&error) => Ok(0),
            Err(error) => Err(format!("reading the terminal: {error}")),
        }
    }

    pub(crate) fn paint(
        &mut self,
        frame: &Frame,
        shown: &mut Vec<String>,
        color: bool,
    ) -> Result<(), String> {
        paint::paint(frame, shown, &mut self.tty, color)
            .map_err(|error| format!("writing the terminal: {error}"))
    }
}

impl Host for Session {
    fn release(&mut self) -> Result<(), String> {
        self.leave();
        Ok(())
    }

    fn reenter(&mut self) -> Result<(), String> {
        self.enter()
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        let tty = &mut self.tty;
        let written = tty.write_all(bytes).and_then(|()| tty.flush());
        written.map_err(|error| format!("writing the terminal: {error}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.leave();
    }
}

fn transient(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock)
}

fn lock() -> MutexGuard<'static, Option<(Termios, ThreadId)>> {
    SAVED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Only the thread that entered raw mode restores the terminal on a panic. A preview thread
/// that panics reaches the event loop as a disconnected job; the screen stays its own.
fn panicking_thread_owns_terminal(owner: ThreadId) -> bool {
    std::thread::current().id() == owner
}

/// A panic prints its message after the terminal is back, not onto the alternate screen that
/// is about to disappear. The hook is installed once and does nothing outside a session.
fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_after_panic();
            previous(info);
        }));
    });
}

fn restore_after_panic() {
    let mut guard = match SAVED.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    let owns = guard
        .as_ref()
        .is_some_and(|(_, owner)| panicking_thread_owns_terminal(*owner));
    if owns && let Some((saved, _)) = guard.take() {
        restore(&saved);
    }
}

fn restore(saved: &Termios) {
    if let Ok(mut tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") {
        let _ = tty.write_all(LEAVE);
        let _ = tty.flush();
        let _ = termios::tcsetattr(&tty, OptionalActions::Now, saved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_owning_thread_restores_the_terminal() {
        let owner = std::thread::current().id();
        assert!(panicking_thread_owns_terminal(owner));
        let elsewhere = std::thread::spawn(move || panicking_thread_owns_terminal(owner))
            .join()
            .unwrap();
        assert!(!elsewhere);
    }
}
