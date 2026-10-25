# ADR-0210: Own native gate children without leaving the enclosing process group

**Status:** proposed (2026-10-25).

## Context

A native gate can run beneath an installed AF subreaper. Orphans from its tests then
accumulate as zombies until the complete gate ends. A dedicated source-level owner can
reap them promptly without replacing the installed binary. Giving the gate body a new
session, however, defeats CheckRunner's immediate process-group SIGKILL: an ordinary
orphan that execs with an empty environment escapes both that group and the outer
supervisor's inherited tree marker. Python's ignored SIGPIPE also survives a plain exec.

## Decision

`verify.sh` execs a disposable, single-threaded Linux subreaper, `gate-owner.py`, around
the unchanged complete gate body. The body inherits the enclosing supervisor's process
group. Ordinary descendants, including environment-cleared orphans, remain reachable by
its hard kill. The owner never signals a group: direct invocation can share an interactive
shell's group, and cooperative cleanup must leave the owner alive until it finishes.

The owner observes only its direct children. It promptly reaps terminal adopted children
while the leader lives, leaving live orphan work alone during normal execution. It does
not steal the body's direct-child statuses. An unreaped leader reserves its PID and exact
exit status through cleanup. Every child signal first uses non-reaping `waitid` to prove
parenthood; only this process explicitly reaps, so a signalled PID cannot have been reused.
Completion requires a terminal leader and a fresh empty adopted-child scan after sweeping,
covering parents that expose another generation during the sweep. A zombie thread leader
with live threads is not terminal until the kernel allows a wait.

After leader exit, live children have ten seconds to drain. Expiry fails the gate and
starts bounded termination. SIGINT, SIGTERM, SIGHUP and SIGQUIT latch the first signal and
forward it to the reserved leader and adopted children. After two seconds, cleanup sends
SIGKILL to these identities, including newly adopted generations, with two further seconds
to confirm exit and reaping. Observation failures refuse success and attempt bounded
emergency cleanup. Leader exit/signal status and streams are retained; cancellation reports
its original signal only after cleanup. Spawn failures remain distinct (126/127).

On non-Linux systems the owner execs the original command, restoring SIGPIPE and available
SIGXFZ/SIGXFSZ to default first, matching Python subprocess `restore_signals` semantics.
There is no Linux subreaping or additional descendant guarantee on those systems.

## Relationship to ADR-0026 and limits

[ADR-0026](0026-share-process-supervision-through-a-leaf-crate.md) continues to own shared
Rust supervision, group creation/killing, deadlines, input and pipe draining. This narrow
repository gate adapter owns only the gate's adopted-child lifecycle, including when run
directly or by an older installed AF. It neither forks that shared supervisor nor creates
a competing process group, changes the gate's resource limits, or supplies containment.

SIGKILL cannot be handled by the owner. The enclosing supervisor must reap children adopted
after its group kill. Descendants that deliberately detach and remove their inherited
marker can still escape that outer cleanup; direct invocation has no such outer guarantee.
Linux requires working `prctl`, `/proc` and child-wait observations. Unconfirmed cleanup is
a failure, not proof of absence. Tests use real isolated outer subreapers, including the
predecessor-failing SIGKILL path, and actual fallback exec signal termination on Linux;
native non-Linux execution remains a platform validation obligation.
