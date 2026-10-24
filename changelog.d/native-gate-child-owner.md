The native gate now has a dedicated Linux child owner around its complete execution.
It promptly reaps exited orphans while tests run, so an outer AF subreaper does not
accumulate gate zombies. Each owner waits only for its own children and reserves the
gate leader's status; normal execution lets live descendants finish. After leader exit,
live descendants have ten seconds to drain before the gate fails and performs bounded
termination. Cancellation forwards the signal, then escalates after two seconds and
allows two seconds to confirm cleanup. An unconfirmed cleanup is reported as a failure.
This is lifecycle management, not containment; an uncatchable kill of the owner still
requires outer-supervisor cleanup. Non-Linux hosts execute the original gate body directly.
Tests, native check targets, timing/JUnit output and gate resource budgets are unchanged.
