- Keep completed child Scenario deadlines out of retained driver expiry and wake scheduling.
  Parent waits can continue within their own unchanged elapsed, scope and resume limits after
  restart; completed evidence, dispatch identities and Task accounting remain retained.
- Add a journal-reopen regression covering the completed child and the still-enforced parent
  deadline. Existing native scoped lifecycle tests cover repairs, independent verification,
  one PR, delayed checks, uncertain publication and CLI restart with exact Task accounting;
  physical VM reboot, mid-Worker termination and live paid publication remain untested.
  See the [active-frame decision](../docs/adr/2026-11-17-bound-implementation-driving-to-active-frames.md).
