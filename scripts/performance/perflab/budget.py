"""The dominant remaining deadline a shard carries into every preflight step (ADR-0171).

A shard reads its run's total deadline, tightened by a caller's, once, before its preflight.
From then on every step that can take time (a Git read of the authority, the container runtime
query, the scratch filesystem's enforcement fill) is given at most the time left before that
deadline less a reserve kept for cleanup and the terminal record, and is not started when none
is left. A step the deadline ended is reported as the deadline's, never as the step's own
failure; a later incomplete verdict is not what bounds it.
"""
from contextlib import contextmanager
import signal
import threading

from .values import ContractError, require


class Interrupted(BaseException):
    """An absolute deadline interruption, never swallowed as a per-item refusal."""


class Expired(ContractError):
    """The run's deadline left a step no time, or ended it."""


@contextmanager
def defer_alarm():
    """Defer the deadline signal only across descriptor ownership transfer/close.

    No waits, flock, traversal or cleanup work belongs here. Restore the caller's exact
    mask; a pending deadline is delivered on restoration, with ownership already recorded.
    """
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGALRM})
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)


class Budget(object):
    """`deadline` and `clock` are instants of one clock; `reserve` is kept free before it."""

    def __init__(self, deadline, clock, reserve):
        self.deadline = deadline
        self.clock = clock
        self.reserve = reserve

    def left(self):
        """Seconds a step may still take: before the deadline, less the reserve."""
        return self.deadline - self.reserve - self.clock()

    def seconds(self, step, wanted=None):
        """The time `step` may take: at most `wanted`, and ending the reserve before the
        deadline. Refused (`Expired`) before the step starts when nothing is left."""
        left = self.left()
        if left <= 0:
            raise Expired("the run's total deadline leaves no time for %s beyond the %d s kept "
                          "for cleanup and the terminal record; it was not started"
                          % (step, self.reserve))
        return left if wanted is None else min(wanted, left)

    def until(self, step, wanted, now=None):
        """The instant `step` must end by: at most `wanted` seconds after `now` (the clock's
        reading unless given), and the reserve before the deadline. Refused like `seconds`."""
        now = self.clock() if now is None else now
        left = self.deadline - self.reserve - now
        if left <= 0:
            raise Expired("the run's total deadline leaves no time for %s beyond the %d s kept "
                          "for cleanup and the terminal record; it was not started"
                          % (step, self.reserve))
        return now + min(wanted, left)

    def expired(self, step, seconds):
        """The refusal of a step the deadline ended after `seconds`."""
        return Expired("the run's total deadline ended %s after %.3f s, %d s before the "
                       "deadline, which is kept for cleanup and the terminal record"
                       % (step, seconds, self.reserve))

    @contextmanager
    def enforce(self, step):
        """Interrupt synchronous trusted collection work at this budget's absolute limit.

        Unix main-thread only, with no existing interval timer. No worker, credential
        transfer, alternate code loader or process ownership boundary is introduced. Git
        receives this same Budget for its own subprocess timeout/kill/wait path.
        """
        require(threading.current_thread() is threading.main_thread()
                and hasattr(signal, "setitimer"),
                "bounded collection needs a Unix main thread with setitimer")
        require(signal.getitimer(signal.ITIMER_REAL) == (0.0, 0.0),
                "bounded collection cannot replace another owner's interval timer")
        seconds = self.seconds(step)
        previous = signal.getsignal(signal.SIGALRM)

        def interrupt(signum, frame):
            raise Interrupted(str(self.expired(step, seconds)))

        signal.signal(signal.SIGALRM, interrupt)
        try:
            signal.setitimer(signal.ITIMER_REAL, seconds)
            yield
            self.seconds(step)
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)
