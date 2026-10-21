"""The dominant remaining deadline a shard carries into every preflight step (ADR-0171).

A shard reads its run's total deadline, tightened by a caller's, once, before its preflight.
From then on every step that can take time (a Git read of the authority, the container runtime
query, the scratch filesystem's enforcement fill) is given at most the time left before that
deadline less a reserve kept for cleanup and the terminal record, and is not started when none
is left. A step the deadline ended is reported as the deadline's, never as the step's own
failure; a later incomplete verdict is not what bounds it.
"""
from .values import ContractError


class Expired(ContractError):
    """The run's deadline left a step no time, or ended it."""


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
