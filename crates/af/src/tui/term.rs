//! The terminal session: `/dev/tty` in raw mode on the alternate screen, restored on every exit
//! path — a normal return, an error, a hand-off to `$EDITOR`, and a panic.
//!
//! Raw mode keeps `VMIN = 0, VTIME = 1`, so a read returns within a tenth of a second whether
//! or not a key arrived. That is the event loop's only clock: background work is polled
//! between reads, and a key is never waited on longer than that.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, Once, PoisonError, TryLockError};
use std::thread::ThreadId;

use nix::errno::Errno;
use nix::sys::signal::{self, SigSet, SigmaskHow, Signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use rustix::termios::{self, OptionalActions, QueueSelector, SpecialCodeIndex, Termios};

use super::paint::{self, Frame};
use super::{Exit, HandOff, Host};

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
    /// A hand-off could not make the terminal safe to take back: the browser stays off it and
    /// ends, leaving the terminal to the shell as any finished job does.
    lost: bool,
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
            lost: false,
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

    /// Leave the browser's screen and cook the terminal in one step: from raw mode straight to
    /// cooked, with or without the keys that raise signals, never through a moment of cooked
    /// mode with signals the browser did not ask for.
    ///
    /// A failure is returned, so a hand-off never starts on a terminal that did not leave; the
    /// session is inactive only once the mode changed, so `enter` can take the screen back.
    fn leave(&mut self, signals: bool) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        let tty = &mut self.tty;
        tty.write_all(LEAVE)
            .and_then(|()| tty.flush())
            .map_err(|error| format!("leaving the browser's screen: {error}"))?;
        termios::tcsetattr(&self.tty, OptionalActions::Now, &self.cooked(signals))
            .map_err(|error| format!("restoring the terminal: {error}"))?;
        self.active = false;
        *lock() = None;
        Ok(())
    }

    /// The saved cooked mode, with or without the keys that raise signals.
    fn cooked(&self, signals: bool) -> Termios {
        let mut cooked = self.saved.clone();
        if !signals {
            cooked.local_modes.remove(termios::LocalModes::ISIG);
        }
        cooked
    }

    /// Closing is best effort: there is nothing left to hand the terminal to.
    pub(crate) fn close(mut self) {
        let _ = self.leave(true);
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

    /// What has been typed so far, without waiting for more.
    pub(crate) fn read_ready(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        match rustix::io::ioctl_fionread(&self.tty) {
            Ok(0) => Ok(0),
            Ok(_) => self.read(buffer),
            Err(error) => Err(format!("reading the terminal: {error}")),
        }
    }

    pub(crate) fn paint(
        &mut self,
        frame: &Frame,
        shown: &mut Vec<String>,
        palette: paint::Palette,
    ) -> Result<(), String> {
        paint::paint(frame, shown, &mut self.tty, palette)
            .map_err(|error| format!("writing the terminal: {error}"))
    }
}

impl Session {
    /// Take the foreground back after a hand-off, or give up the terminal: a browser still in
    /// the background must not touch it again (re-entering would stop it with `SIGTTOU`), so
    /// it ends and leaves the terminal to the shell.
    fn retake(&mut self, group: termios::Pid) -> Result<(), String> {
        self.take_foreground(group).map_err(|error| {
            self.lost = true;
            format!("{error}; the browser leaves the terminal to the shell and ends")
        })
    }

    /// Give the terminal's foreground back to the browser's process group. The browser is in
    /// the background until this returns, so `SIGTTOU` is blocked for the call: blocked, the
    /// change is allowed and nothing stops the browser.
    fn take_foreground(&self, group: termios::Pid) -> Result<(), String> {
        let taken = from_the_background(|| termios::tcsetpgrp(&self.tty, group))
            .map_err(|error| format!("taking the terminal back: {error}"))?;
        taken.map_err(|error| format!("taking the terminal back: {error}"))
    }
}

impl Session {
    /// Cook the released terminal with or without the keys that raise signals.
    fn cook(&self, signals: bool) -> Result<(), String> {
        termios::tcsetattr(&self.tty, OptionalActions::Now, &self.cooked(signals))
            .map_err(|error| format!("restoring the terminal: {error}"))
    }
}

impl Host for Session {
    fn release(&mut self) -> Result<(), String> {
        // `<C-c>` is a byte, not a signal to the browser's group, until `run` hands the
        // foreground to the command or `signals` asks for them.
        self.leave(false)
    }

    fn reenter(&mut self) -> Result<(), String> {
        if self.lost {
            return Err("the terminal was left to the shell".to_owned());
        }
        self.enter()
    }

    /// The child runs in its own process group, which owns the terminal's foreground until it
    /// ends: `<C-c>` and `<C-\>` signal the child, never the browser.
    fn run(&mut self, child: &HandOff) -> Result<Exit, String> {
        use std::os::unix::process::CommandExt as _;
        // The command must own the foreground apart from the browser: without the browser's own
        // group there is no way to keep `<C-c>` from the browser, so nothing runs.
        let browser = termios::tcgetpgrp(&self.tty)
            .map_err(|error| format!("reading the terminal's foreground: {error}"))?;
        let mut command = Command::new(&child.program);
        command.args(&child.args).current_dir(&child.dir);
        command.envs(child.env.iter().map(|(name, value)| (name, value)));
        command.process_group(0);
        let spawned = command
            .spawn()
            .map_err(|error| format!("{}: {error}", child.program.display()))?;
        let raw = i32::try_from(spawned.id()).map_err(|error| error.to_string())?;
        let pid = Pid::from_raw(raw);
        let handed = termios::Pid::from_raw(raw).ok_or("the command has no process id")?;
        if let Err(error) = termios::tcsetpgrp(&self.tty, handed) {
            // A fast command may have ended before its group could take the foreground: its
            // group is gone, and what it did is its exit, not a failed hand-off. The browser
            // never gave the foreground away, so nothing is taken back.
            match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(_, code)) => return Ok(Exit::Code(code)),
                Ok(WaitStatus::Signaled(_, signal, _)) => {
                    return Ok(Exit::Signal(signal as i32));
                }
                _ => {}
            }
            let _ = signal::killpg(pid, Signal::SIGKILL);
            let _ = wait_for(pid);
            return Err(format!("handing the terminal to the command: {error}"));
        }
        // The command's group owns the foreground: its keys may raise signals now, and they
        // reach the command, never the browser. The browser is in the background.
        let cooked = from_the_background(|| self.cook(true))
            .map_err(|error| format!("restoring the terminal: {error}"))
            .and_then(|cooked| cooked);
        // A child that touched the terminal before its group owned the foreground was stopped
        // for it; it continues now that it does.
        let _ = signal::killpg(pid, Signal::SIGCONT);
        if let Err(error) = cooked {
            // Signals are still off: taking the foreground back is safe.
            let _ = signal::killpg(pid, Signal::SIGKILL);
            let _ = wait_for(pid);
            self.retake(browser)?;
            return Err(error);
        }
        let exit = wait_for(pid);
        // `wait_for` reaped the process; the handle only names it.
        drop(spawned);
        // Signals off before the browser owns the foreground again, so no key raises one in the
        // browser between here and its raw session. A terminal that refuses is not taken back
        // at all: the browser ends without touching it, and the shell takes it back.
        let quiet = from_the_background(|| self.cook(false))
            .map_err(|error| format!("restoring the terminal: {error}"))
            .and_then(|quiet| quiet);
        if let Err(error) = quiet {
            self.lost = true;
            return Err(format!(
                "{error}; the browser leaves the terminal to the shell and ends"
            ));
        }
        self.retake(browser)?;
        exit
    }

    fn pause(&mut self, line: &str) -> Result<(), String> {
        // A fresh line whatever the command left: a line's width of spaces wraps only when the
        // cursor was mid-line, and the carriage return then starts the line after its output;
        // at the start of a line the spaces stay on it and nothing is added.
        let (columns, _) = self.size();
        self.send(format!("{}\r", " ".repeat(columns)).as_bytes())?;
        self.send(line.as_bytes())?;
        // Keys typed before the line was shown were meant for the command, not for this wait.
        let _ = termios::tcflush(&self.tty, QueueSelector::IFlush);
        let mut raw = self.saved.clone();
        raw.make_raw();
        raw.special_codes[SpecialCodeIndex::VMIN] = 1;
        raw.special_codes[SpecialCodeIndex::VTIME] = 0;
        *lock() = Some((self.saved.clone(), std::thread::current().id()));
        termios::tcsetattr(&self.tty, OptionalActions::Now, &raw)
            .map_err(|error| format!("entering raw mode: {error}"))?;
        let mut byte = [0_u8; 1];
        let outcome = loop {
            match self.tty.read(&mut byte) {
                Ok(0) => break Err("the terminal closed".to_owned()),
                Ok(_) if matches!(byte[0], b'\r' | b'\n') => break Ok(()),
                Ok(_) => {}
                Err(error) if transient(&error) => {}
                Err(error) => break Err(format!("reading the terminal: {error}")),
            }
        };
        // Back to the released mode, signals still off: `reenter` goes raw next.
        let _ = termios::tcsetattr(&self.tty, OptionalActions::Now, &self.cooked(false));
        *lock() = None;
        outcome
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        let tty = &mut self.tty;
        let written = tty.write_all(bytes).and_then(|()| tty.flush());
        written.map_err(|error| format!("writing the terminal: {error}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.leave(true);
    }
}

/// Run a terminal call the browser makes while another process group owns the foreground. It
/// would raise `SIGTTOU`, which stops the browser; with the signal blocked, the call is allowed.
fn from_the_background<T>(call: impl FnOnce() -> T) -> Result<T, Errno> {
    let mut blocked = SigSet::empty();
    blocked.add(Signal::SIGTTOU);
    let mut previous = SigSet::empty();
    signal::pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut previous))?;
    let outcome = call();
    let _ = signal::pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
    Ok(outcome)
}

/// Wait for a handed-off child to end. A child stopped by `<C-z>` is continued: the browser
/// has no job control, and a stopped command would hold the terminal with nobody to resume it.
fn wait_for(pid: Pid) -> Result<Exit, String> {
    loop {
        match waitpid(pid, Some(WaitPidFlag::WUNTRACED)) {
            Ok(WaitStatus::Exited(_, code)) => return Ok(Exit::Code(code)),
            Ok(WaitStatus::Signaled(_, signal, _)) => return Ok(Exit::Signal(signal as i32)),
            Ok(WaitStatus::Stopped(..)) => {
                let _ = signal::killpg(pid, Signal::SIGCONT);
            }
            Ok(_) | Err(Errno::EINTR) => {}
            Err(error) => return Err(format!("waiting for the command: {error}")),
        }
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

    /// A terminal whose modes cannot be changed: the release fails and says why, and the
    /// session stays active, so re-entering takes the screen back.
    #[test]
    fn a_release_the_terminal_refuses_is_an_error() {
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize::default())
            .unwrap();
        let name = pair.master.tty_name().unwrap();
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .unwrap();
        let saved = termios::tcgetattr(&slave).unwrap();
        // `/dev/null` takes the leave sequence but is no terminal: `tcsetattr` fails.
        let tty = OpenOptions::new().write(true).open("/dev/null").unwrap();
        let mut session = Session {
            tty,
            saved,
            active: true,
            dirty: false,
            lost: false,
        };
        let error = session.release().unwrap_err();
        assert!(error.starts_with("restoring the terminal: "), "{error}");
        assert!(
            session.active,
            "a failed release leaves the session to re-enter"
        );
        session.active = false;
    }
}
