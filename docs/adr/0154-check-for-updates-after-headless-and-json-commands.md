# ADR-0154: Check for updates after headless and `--json` commands

**Status:** accepted (2026-10-19). Supersedes in part
[ADR-0044](0044-af-manages-itself-and-dispatches-to-the-pinned-release.md): clause 4's "on a
TTY, never under … `--json`" for scheduling the check. The notice it prints stays interactive.

## Context

ADR-0044 spawns the detached update check only after a command run on a terminal, and never
under `--json`. `af` is now mostly run by agents and scripts: headless, with captured pipes, often
with `--json`. With the default policy (`update_check = true`, `auto_update = "notify"`) such a
machine never checked at all, so `af self status` never learned of a newer release and the cached
notice had nothing to show when a person did run `af` at a terminal. Inside a project that pins
a release it was worse: dispatch execs the pinned release, so the dispatching af never reached
its after-command step, and the dispatched one returns early because it carries
`AF_DISPATCHED_FROM`.

The terminal condition guarded two different things. Printing a notice into output a program
reads is wrong. Spawning a check that holds none of the command's pipes, prints nothing, and
cannot change the exit status is not.

## Considered options

- **Keep the check TTY-only.** Headless and agent machines never check. This is the bug.
- **Run the check in the foreground for headless commands.** Puts the network on the hot path
  and lets a slow or failing release source change a command's latency or result.
- **A timer, daemon, cron entry or systemd unit.** A second thing to install, supervise and
  remove, for a check that is already throttled by `check_every`.
- **Let the pinned release schedule its own check.** An older pinned release cannot be changed,
  and its version is not the machine default's, so `always` would compare against the wrong
  binary. Reading `AF_DISPATCHED_FROM` to guess what an older dispatcher did would be a fallback
  for an earlier release's behaviour, which ADR-0145 forbids.
- **Schedule from every eligible command, terminal or not; notify only interactively.** Chosen.
- **Let `af self uninstall` wait for a running check.** A foreground command would then wait on
  the release source. Refusing at once, and saying to run it again, is honest and bounded.
- **Have `af self update` and `rollback` clear the "updated the default" record.** They would need
  the check lock, which a running check may hold and then write back with the stale record. A
  record bound to the activation it announces needs no such coordination.

## Decision

1. **Scheduling is independent of the terminal and of `--json`.** After every eligible command,
   af spawns `af self refresh-check` when the cache is at least `check_every` old. Eligible
   means: not exempt from dispatch (`help`, `--version`/`--help`, `af self`, `af config`,
   `completions`, the browser and the machine-local `af provider` commands keep their
   exemptions), not `AF_SELF_OFFLINE`, not `CI`, `update_check = true`, and
   `auto_update` other than `never`. `check_every` throttles invocations; nothing runs between
   them.
2. **The child is detached and best effort.** Its stdin, stdout and stderr are null and it runs
   in its own process group, so it holds no captured pipe and needs no terminal. The command does
   not wait for it, and its failure never changes the command's output or exit status.
3. **The notice is interactive only.** The cached "newer release" or "updated the default"
   line goes to stderr only when stderr is a terminal and the run has no `--json`. A headless run
   leaves the "updated the default" record in the cache for the next interactive run. Under a
   pin the af that dispatches shows it, before it execs, on the same conditions: the pinned
   release never reaches the after-command step that would.
4. **Under a pin, the af that dispatches schedules the check, once.** Just before it execs the
   pinned release it schedules the check for that command, as itself: the check compares against
   the machine's own af, and `always` retargets the default, never the lock or the pinned
   version. An af that runs with `AF_DISPATCHED_FROM` never schedules, so no command is checked
   twice, and a process that scheduled once (a dispatch whose exec failed) does not schedule
   again. `af self refresh-check` is itself exempt, so the check never recurses.
5. **Overlapping commands share one check.** The child takes a non-blocking lock beside the cache
   (`$XDG_CACHE_HOME/af/self/latest.lock`); a child that finds it held exits at once. It reads
   and writes the cache only under that lock, and once it holds the lock it reads the cache
   again and exits unless the check is still due: two commands that read the same stale cache
   spawn two children, and the later one asks nothing when the earlier one has answered, failed
   query included. It records `last_check` in the self state under the versions lock that every
   other writer of that state holds, so a check never drops an activation or a seen pin written
   meanwhile. Dropping a seen pin whose lock is gone is such a write too: it reads the state
   again under that lock, and a caller that already holds the lock, the storage sweep, does so
   without taking it twice. Recording the seen pin of the release that is running already stays
   best effort, but a dispatch that cannot take the versions lock records nothing rather than
   write the state without it. A dispatch that execs another release needs the pin (clause 12).
6. **Consuming the notice never undoes a check.** An interactive run reads the cache for its
   notice, and clears the "updated the default" record once it has printed it, only under the
   same lock, taken without waiting and before it spawns its own check. While a check holds the
   lock, it prints neither notice and leaves the cache untouched, so the record waits for a later
   interactive run and the check's newer timestamp, release and record are never overwritten.
   The record is printed only once the cache without it is written: when that replacement
   fails, nothing is printed and the record stays for a later run, so it is never printed twice.
   `af self update`, the cache's one other writer, records the release it found under the same
   lock, taken the same way, and leaves the cache to a check that holds it. An "updated the
   default" record carries the mark of clause 8 as the activation left it, and is printed only
   while the default still carries that mark. An `af self update` or `rollback` made since the
   check is the operator's, so the record it made stale is cleared without being printed: a
   rollback, and an update back to the version the record names too, because each records
   another activation and replaces the link. The mark is read without the versions lock, so
   showing a notice never waits for it; a change in progress reads as a change.
7. **A failed attempt is throttled like a good one.** A check whose release query fails still
   records the attempt time in the cache and keeps what the last good check found, so an outage
   costs at most one query per `check_every`. Every failure after the attempt begins is recorded
   the same way, still under the check lock, before the check returns its error: a query that
   answered but whose `last_check` the self state could not take (an unwritable state path)
   leaves the attempt time and the release it found, so a broken state never turns every
   command into a query. Under `always`, a failed installation or activation is recorded the
   same way and is never announced as an update; `last_check` in the self state still names
   only completed queries.
8. **No command waits for a background download.** An installation, the one `always` starts in
   the child included, fetches, verifies and proves the release in a staging directory without
   the versions lock. It holds that lock only to look for an installed copy first and to publish
   the verified one with one rename, after looking again: a copy another installer published
   meanwhile stands, and under a project lock it must still carry the lock's digest. A command
   that records a seen pin, activates a version or sweeps storage therefore waits at most for
   that short publication, never for the release source. A staging directory that was not
   published is removed.
   Under `always`, the child marks the default under the versions lock, in the critical section
   that records `last_check`, before it installs: its version, how many activations the self
   state records, and the default link's identity (device, inode and change time). Under the
   versions lock again it retargets the default only if that mark is unchanged: the comparison
   and the retargeting are one critical section. Every activation that changes the version
   records one more activation and replaces the link, so an `af self update` or `rollback` made
   meanwhile is seen even when it ends at the version the child read, and even when the new link
   reuses the old one's inode number. It is the operator's choice and stands; the release stays
   installed, and no "updated the default" record is written. An activation that happens records
   the mark it left in its "updated the default" record (clause 6). That changes the cache's
   shape, so under ADR-0145 the cache is versioned: `schema = "af/self-check@2"`, unknown keys
   refused. The unversioned cache earlier releases wrote is read as no cache, so the first command
   after an upgrade is due a check and its pending notice is dropped.
   `af self remove` and `af self prune` hold the same versions lock while they read the default
   and the pins and while they delete: a protection read before the lock is not trusted. A
   version a background activation makes the default, or a dispatch pins, while a removal waits
   is therefore kept, and the default never points at a deleted release. They read the pins with
   the lock-held helper, so they never wait on themselves, and they touch no release source.
9. **Tests stay off the network and deterministic.** The integration harness's `af()` sets
   `AF_SELF_OFFLINE`; tests of the check use a directory release source and private XDG
   directories. A debug build appends one line per spawned check to the file
   `AF_TEST_SELF_CHECK_LOG` names, so a fixture knows when a command returns whether it
   scheduled a check; a release build never reads that setting.
10. **An interrupted installation's staging is recovered and counted.** An installer creates its
    staging directory (`versions/.<version>.<pid>`) under the versions lock and holds it locked
    with `flock` until it ends; the kernel releases that lock when the process dies. Under the
    versions lock, before it stages, every installer removes the staging directories whose lock
    it can take and whose named process no longer exists. `install.sh` stages without a lock,
    so the process check keeps its directory; a directory whose lock or process cannot be asked
    about is kept. Until a staging directory is removed, the Storage Budget counts it as an
    installed version: a live one as in use, an abandoned one as evictable, judged again under the
    versions lock before it is removed.
11. **An uninstall never races a check.** `af self uninstall`, with or without `--purge`, takes
    the check lock without waiting and refuses while a check holds it, since that check may be
    fetching a release to install: an honest refusal, never a wait on the release source. It
    holds the check lock to the end. Under the versions lock it refuses while an installer is
    staging, then removes the default link and the versions. It releases that lock before the
    purge, which may take long. An installer keeps open the versions directory it found, and
    under the versions lock it stages and publishes only while that directory is still the one
    at the versions path. One that fetched while an uninstall removed the tree therefore
    installs nothing and recreates nothing, and the open directory's identity cannot be reused
    meanwhile. The command that schedules a check creates the check lock's file. The check never
    creates it, and refuses a lock file a purge unlinked while it opened it. Under the lock it
    leaves without writing when its own binary is gone, as an uninstall leaves it. So a check
    scheduled before an uninstall and started after one writes nothing: not the cache, the state,
    or the versions.
    A command that was running when an uninstall began recreates nothing either. Its notice takes
    the check lock as the check does, never creating the file, and the uninstall holds that lock
    while it purges. It creates the lock's file for the check it schedules only under the versions
    lock, taken in the versions directory as it stands and never creating it, and only while its
    own binary is still there. The uninstall removes the versions, and the self-managed binary
    in them, under that lock before it purges. So the file is made before the teardown, which
    then removes it, or the binary is found gone and nothing is made. Checking for the binary and
    then creating the file, without that lock, could run either side of a whole purge. The
    command takes that versions lock without waiting: while a prune deletes releases, an
    installation publishes one or an uninstall tears the versions down, it schedules nothing and
    leaves the check to the next eligible command, so a finished command never waits for them.
    A binary another installer put outside the versions is not one an uninstall removes, so it
    schedules as before.
12. **A dispatch pins what it runs in one critical section.** Under a project lock, dispatch
    looks for the pinned release, and installs it when needed, with the versions lock taken
    only briefly and never across the release source. It then takes the versions lock once more.
    In that one critical section it finds the release still installed with the lock's digest
    and records the seen pin, the helper that assumes the lock already held, so it never waits
    on itself. A `remove` or `prune` therefore either deleted the release before that critical
    section, and dispatch looks for it once more, installing it again where the policy allows,
    or finds the pin and keeps it. A release that is gone twice is treated like one that cannot
    be installed: it fails closed or runs this binary, as before. Bytes that are no longer the
    lock's fail closed. A release is run only once its pin is in the self state. When the
    versions lock cannot be taken, or the self state cannot be written, nothing keeps the release
    from `remove` or `prune`. Dispatch then fails closed, saying why on stderr, and never execs
    the release unprotected.

## Consequences

- Agents and CI-less automation now learn of new releases on the same cadence as a person at a
  terminal: at most one release-source query per `check_every`, after a command, even while the
  source is down.
- `auto_update = "always"` now also applies after headless commands. It still installs only
  verified bytes and only retargets the default; the running process and every project pin stay
  untouched, as ADR-0044 and ADR-0045 require.
- A pinned release older than this one, run directly with `AF_DISPATCHED_FROM` set by an older
  dispatcher, is not checked: neither binary schedules. The machine's next unpinned command, or
  its next dispatch by this release or a later one, checks.
- A killed installer's bytes stay within the budget: they are counted until the next
  installation recovers them or a sweep evicts them.
- `af self uninstall` may refuse for as long as one check runs; running it again afterwards
  succeeds. An installation that was fetching while an uninstall removed the versions fails and
  says so, instead of recreating them.
- The update-check cache is `af/self-check@2`. A cache an earlier release wrote, or one a pinned
  older release writes, is read as no cache, so the next eligible command is due a check, which
  writes the current shape.
- The self state's versions lock directory may now exist on a machine that never installed a
  release through `af self`; it holds only the lock file.
