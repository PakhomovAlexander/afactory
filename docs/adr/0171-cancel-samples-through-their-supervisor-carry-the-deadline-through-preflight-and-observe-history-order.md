# ADR-0171: Cancel samples through their supervisor, carry the deadline through preflight, and observe history order

**Status:** accepted (2026-10-20); decisions 3 and 4 amended by
[ADR-0172](0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md)
(the deadline before artifact admission, and streamed JSON in place of the history's prefix
observation), and decisions 1 and 3 by
[ADR-0173](0173-own-the-supervisor-a-test-runs-and-bound-admission-collection-scratch-and-cancellation-by-absolute-deadlines.md) (the cancellation's launch gate, a container's one removal and one absolute
deadline for the scratch check). Closes the lifecycle and verification findings of packages p2
and p3 of the [continuous performance laboratory](../design/continuous-performance.md),
against the sealed snapshot of
[ADR-0170](0170-seed-native-fixtures-freeze-the-build-image-and-bound-the-run-by-one-deadline.md).
Its decisions stand except where this ADR amends them:

- decision 4 (the host backstop and the total deadline);
- decision 6 (the total deadline now bounds the preflight too);
- decision 5 of
  [ADR-0168](0168-run-frozen-shards-with-whole-cli-oracles-and-only-a-per-sample-scratch.md)
  (the `task show` rules).

The new contract versions are:

| Contract | Version | What changed |
| --- | --- | --- |
| Supervisor request | `af.perf-supervisor-request/3` | `stdout_sequences` prefixes |
| Supervisor result | `af.perf-supervisor-result/4` | their in-order observations |
| CLI registry | `af.perf-cli/4` | the `stdout_sequence` rule |

## Context

Root checked three findings against the sealed predecessor:

- **F1, a total deadline left the measured child running.** `supervise.launch` shortened its
  host backstop to the run's remaining time and, when that expired, `subprocess.run` killed
  the supervisor. In trusted local mode the child leads its own process group, so it kept
  running. With the default margin and a 0.5 s deadline, the call returned after 0.505 s, with
  the child still running. The runner then removed the sample's private scratch beneath it.
  A supervisor that replied with garbage, or exited without a reply, was treated the same way.
- **F2, the preflight did not carry the deadline.** A shard checked its run deadline once,
  before its preflight, and set its effective deadline only when it began sampling. With a
  controlled clock, 0.5 s remained when the shard was admitted. A stand-in Docker query took
  2 s, the scratch capability check began after the deadline had passed, and preparation
  returned 5.5 s past it. Neither the runtime query nor the enforcement fill received the
  remaining time.
- **F3, the `task show` oracle accepted wrong history.** Its rules counted `{"sequence":`
  markers and checked the first and last entry and 9,997 lease renewals. 10,000 entries with
  only 4 distinct numbers passed, and so did 10,000 unique numbers with the middle reversed.
  The roughly 1.6 MB document is larger than any retained log, so nothing else could see the
  order. These are oracle counterexamples, not observed product failures.

The predecessor's mandatory gate also ended with 2,095 tests passed, 14 skipped and 7 ended by
SIGTERM, all in `task_campaign_review::host` of `review-pipeline`: four in `integration`
and three in `native`. Its review could not bind: the 203-path delta encoded to 4,270,714
bytes, above the 4 MiB ChangeSet bound, mostly because regenerated sample fixtures were
included.

## Considered options

1. **Shorten the host bound and kill the supervisor, as before.** Rejected: that is F1. The
   host cannot name the child's process group, and in trusted local mode nothing else ends it.
2. **Have the host kill the child's group itself.** Rejected: the host would need the child's
   pid from a supervisor that has not replied. It would also be a second child lifecycle
   beside the shared supervision of
   [ADR-0026](0026-share-process-supervision-through-a-leaf-crate.md).
3. **Cancel through the supervisor.** Chosen. The supervisor already forwards SIGTERM to its
   sample as a cancellation. It signals the child, kills it after its cleanup grace, verifies
   it gone, and replies with how the sample ended.
4. **Refuse every launch whose own deadlines exceed the remaining time.** Rejected as the only
   rule. The shard already checks each slot's worst case before starting it. A second,
   different bound inside `launch` would only turn admitted slots into failures. `launch`
   refuses only what it could not cancel in time.
5. **Check the deadline only once, before the preflight.** Rejected: that is F2.
6. **Bound each preflight step by the remaining time.** Chosen. The fill keeps every
   enforcement and error check; only its time bound shortens.
7. **Keep counting markers, and add a digest of the whole output.** Rejected: a digest freezes
   formatting and payload fields the fixture does not declare.
8. **Retain more output.** Rejected: the sample log's 1 MiB maximum is fixed.
9. **Have the supervisor observe the number after each marker over the whole stream.** Chosen.
   It is the same streaming observation as the pattern counts, so the retained log stays
   bounded.

## Decision

1. **A sample is cancelled through its supervisor.**
   - **The bound.** The host bound is the backstop of ADR-0170, ending before the run's total
     deadline by the request's cancellation reserve. The reserve is three cleanup graces (the
     forwarded signal, the kill and the verification) plus 10 s for the reply.
   - **Refusal.** Nothing is launched once the remaining time is less than the reserve plus 1
     s. That second is for the supervisor's start, before it takes over termination signals.
     The refusal says that nothing was launched, and a shard records it as `interrupted` with
     the deadline that bound it.
   - **Cancellation.** When the bound expires, the supervisor is sent SIGTERM, never killed. It
     cancels whatever is running: a setup that is cancelled launches no measured child. The
     shared leaf (`review_process::sample`) now launches nothing once a cancellation has
     arrived, so a cancellation between the setup and the child launches no child. The reply
     is never judged. Its lifecycle says whether the sample's end was confirmed.
   - **No reply.** A supervisor that does not reply within the reserve is killed. In a
     container, the container's confirmed removal ends every process in it. Without a
     container, nothing confirms that the sample ended.
   - **Other failures.** A supervisor that exits without a valid reply is treated the same
     way. The exceptions are its refusal (exit 2 without stdout) and death by the signal before
     it took the signal over, both of which come before anything is launched.
2. **What may be live is kept.**
   - **When.** In trusted local mode, a sample's end is unconfirmed when no reply confirms it,
     or when the reply's lifecycle is `cleanup_uncertain`.
   - **What the shard does.** It stops with `cleanup_uncertain`. It keeps the sample's
     private scratch and the shard's seed store, which such a process could read or write,
     and names them in the failure of `shard.json`. A seed writer's scratch is kept the same
     way.
   - **Containers.** A container whose removal is not confirmed also keeps the seed store.
   - **Diagnostics.** `cli.py probe` keeps its private scratch under the same conditions.
   - **What is never done.** Nothing kept is removed by the laboratory, and no other process
     is signalled.
3. **The run's deadline is carried through the preflight.**
   - **Read once.** The shard reads the run's total deadline, tightened by a caller's, once,
     as an instant of its monotonic clock, before its preflight.
   - **Each step bounded.** Each Git read of the authority, the container runtime query and the
     scratch enforcement fill takes at most its own bound and the time left before 30 s ahead
     of that deadline. None of them, nor the host's CPUs, its identity or the run directory,
     starts once nothing is left. A runtime query or fill the deadline ended is the deadline's
     refusal (`run_deadline`), never `no_container_runtime` or `scratch_unverified`.
   - **Sampling.** The sampling deadline still starts with the samples. The shard's deadline
     is the earlier of it and the run's, which is never reset.
4. **The `task show` history is observed whole.**
   - **The supervisor's observation.** A request may name up to four `stdout_sequences`
     prefixes. For each, the supervisor observes the number after every occurrence over the
     child's entire stdout, past the retained log, across every read boundary. Optional JSON
     whitespace may precede the number, which has at most 20 digits, no leading zero, and fits
     in 64 bits. It reports the occurrences, the first and last number, and how many
     occurrences were not the previous number plus one, with the first of them.
   - **The rule.** `stdout_sequence` requires exactly `first..last`, each once and in order.
     A seeded command's sequence must count exactly its seed's declared events, so the
     expected numbers come from the fixture, never from the output.
   - **`task show`.** It now requires its history entries numbered 0..9999 and keeps 9,997
     lease renewals, the identity and the first entry in the retained prefix. Its three marker
     counts are replaced by the sequence. Documented fields the fixture does not declare stay
     free.

## Consequences

- **Versions.** Earlier versions of the three contracts above are refused with no reader.
  Their schemas are replaced by `performance-supervisor-request-v3.json`,
  `performance-supervisor-result-v4.json` and `performance-cli-v4.json`. The supervisor is
  built from the authority, so no other supervisor speaks to this driver.
- **Unchanged bounds.** The 15-second production lease, the 8-MiB envelope cap, every sample
  timeout, cleanup grace, log cap, pair count, the 2-GiB scratch and its fill are unchanged.
  The fill's enforcement and error checks are unchanged. No unbounded bind or tmpfs replaces
  anything.
- **Real-process tests.** The supervisor's integration test drives the host's own `launch`
  with the real `perf-supervisor` and an owned nonspawning `sleep`:
  - a 0.5 s deadline with the default margins launches nothing;
  - a deadline 14 s away cancels the running child through the supervisor after 10 s, with its
    end confirmed and its recorded pid gone;
  - the same deadline during the setup cancels it, and no measured child launches.
- **Sequence tests.** The same test observes 3,000 entries past a 1,024-byte log: a whole run,
  a duplicate, a gap, a swap and a damaged number.
- **The oracle over the real supervisor.** It judges what the real supervisor observed of a
  10,000-entry history. The document is valid, duplicated, missing an entry, reordered, has an
  extra entry, or is the root's four-number counterexample. Only the valid one passes, from a
  196,608-byte retained prefix.
- **Actual AF output.** The probe's integration test has the side's own probe write the
  10,000-event Task state. The actual `af task show` reads a private copy under the real
  supervisor with the shard's environment and caps, and its whole output passes: 10,000
  entries numbered 0..9999 with no break, and 9,997 renewals.
- **Leaf unit tests.** These cover every chunk split and one byte at a time, malformed numbers,
  a prefix with an overlapping border, and a cancellation before the launch.
- **Python tests.**
  - A supervisor that never replies is killed, and the failure is uncertain.
  - A shard that meets such a supervisor, a garbage reply or an uncertain lifecycle stops and
    keeps exactly the scratch it names.
  - A seed writer that never replies keeps the seed store.
  - A launch too near the deadline is refused and interrupts the shard.
  - Controlled clocks show that a deadline consumed by a slow runtime query starts no fill, and
    that each step receives the time left.
  - The rule refuses duplicated, missing, reordered, extra, damaged and wrongly started
    histories at every retention boundary.
- **The seven SIGTERM failures.** The cause was investigated, not established.
  - **Rerun here.** On the predecessor's unchanged Rust source, the 63 tests of
    `task_campaign_review::host` passed under nextest with seven threads in 61 s. The slowest
    took 36.8 s, far from any limit.
  - **Full suite here.** A full nextest run of that source passed 2,102 tests with 14 skipped
    in 592 s. All seven tests passed and completed next to each other in the run's order. A
    full run of this change's source, with seven threads, ended no test by a signal: 2,110 of
    2,111 passed in 923 s. The one failure was this change's own test path resolution, which the
    workspace's fixture-path check refused; it was fixed and that check passed when rerun.
  - **The test count.** 2,095 + 7 + 14 is the suite's whole count of 2,116, so the
    predecessor's run was not cancelled.
  - **Seven tests.** Seven is `make test`'s thread count on a 14-core host like the
    development host, which fits one moment's whole in-flight set being signalled. That is an
    observation, not a diagnosis.
  - **No sender in the source.** No code these tests run sends SIGTERM. In the workspace,
    SIGTERM is sent only to the test-owned `af` of `task_interrupt`, to the test-owned
    supervisor of `perf_supervisor`, and by the supervisor's forwarding to its own sample.
    Every other group signal in the workspace is SIGKILL or SIGCONT.
  - **Nothing changed for them.** No test, timeout, lease, thread count or gate was changed
    for them. These reruns are not proof of any cause.
- **Delta size.** No sample or comparison fixture was regenerated. The schema fixtures change
  only their version and one empty field, and four small fixtures are added, so the ChangeSet
  stays far below its 4 MiB bound.
- **Not evidence.** None of this is calibrated performance, a hosted x86-64 run or a 2-GiB
  Linux scratch. No container sample ran here.
