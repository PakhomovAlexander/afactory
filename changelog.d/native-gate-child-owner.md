The native gate now has a dedicated Linux child owner around its complete execution.
It promptly reaps exited orphans while tests run, so an outer AF subreaper does not
accumulate gate zombies. Each owner waits only for its own children and reserves the
gate leader's status; normal execution lets live descendants finish. After leader exit,
live descendants have ten seconds to drain before the gate fails and performs bounded
termination. Cancellation forwards the signal, then escalates after two seconds and
allows two seconds to confirm cleanup. An unconfirmed cleanup is reported as a failure.
The body and ordinary descendants retain the enclosing supervisor's process group, so its
immediate SIGKILL reaches even adopted orphans that clear their environment. Cooperative
cleanup signals only unreaped child PIDs, never the enclosing group or unrelated siblings.
This is lifecycle management, not containment; an uncatchable kill of the owner still
requires outer-supervisor reaping, and descendants that detach and clear their marker can
escape that cleanup. Non-Linux hosts execute the original gate body directly after restoring
Python-ignored SIGPIPE and available SIGXFZ/SIGXFSZ to default signal dispositions.
The lifecycle contract and platform limits are recorded in ADR-0218.
Tests, native check targets, timing/JUnit output and gate resource budgets are unchanged.
