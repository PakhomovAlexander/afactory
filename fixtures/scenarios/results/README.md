# Private result examples

`weather.json` and `library.json` are two unrelated executable native Scenarios. Neither has Task
or issue fields. Each exposes named `report` and `empty` outputs. Weather has two author-defined
tables plus prose; library has nested JSON, literal special characters and intentional Markdown.
The observation dates are fixed sample data and correctly render **stale** today: refreshing never
claims a new weather measurement or inventory count. Their only step is conditionally skipped;
there are no models, adapter processes, writes, network requests or external decisions.

Run both through real repeated one-second Ant occurrences, then watch a recorded result at one
second while scheduling is stopped:

```sh
cargo build --locked -p af
python3 -I scripts/measure-ant-intervals.py --af target/debug/af --seconds 10
```

The recipe uses a disposable private HOME and every XDG root, an owner-only copied policy and
an external private Store. It registers only private fixture Ants, initially disabled, measures
idle scheduling, explicitly enables those fixtures, runs a finite foreground scheduler, inspects
unique receipts and successful zero-dispatch Runs, and measures read-only atomic output polling.
Cleanup removes only its own temporary directory. The inert file grant is never dispatched. It
creates no machine configuration or service and never touches a production monitor.

Measurements report exact command argument lists, exit codes, monotonic elapsed time, wait4 CPU,
context switches, runtime/journal bytes and per-occurrence due/admission receipts. macOS also
samples the owned process's unprivileged `proc_pid_rusage` v2 package-idle/interrupt wakeup counters
before exit, with the exact sampling duration. These are process counters, not host-wide wakeups;
other platforms or refusals report unavailable. Startup/inspection costs and a debug build affect
these short runs. Results describe only the supplied binary, sample duration and two inert Ants;
they make no production latency or CPU claim. Native [retention](../../../docs/scenarios.md#native-retention-and-capacity) bounds eligible old Ant
history and exposes what expired; protected work, the existing 16 MiB journal and machine Storage
Budget still apply. The short measurement alone does not prove sustained retention; deterministic
Store tests cross the prior bound.

Focused native tests independently exercise these fixtures through `crate::common::af()`:

```sh
cargo test --locked -p af-scenario --lib
cargo test --locked -p af --test it interval_scheduler_executes_real_one_second_occurrences_and_generic_results
cargo test --locked -p af --test it markdown_watch_interrupt_is_read_only_atomic_and_retains_active_run
```

For a separately authorized recovery Scenario, author a normal Ant with
`{"kind":"interval","seconds":900,"start_unix_ms":YOUR_REVIEWED_ANCHOR}`. Review and confirm its
native pin, explicitly enable it, then explicitly run `af ant scheduler run`. Existing host recovery
adapters and machine policy remain separately authorized. This recipe does not convert or activate
the running recovery monitor, install launchd jobs or send notifications.

`fleet-document.py` is a third author-side example. It reads only an explicitly selected local
Task's native `show`, `status` and exact recorded plan, refusing changed revisions/states during
capture. It emits a reusable result document containing environment/Store/Task identity, the exact
recorded goal (including issue context if captured), native terminal outcome/cause, native elapsed
clock and charged tokens with unknown usage kept separate. Receipt hashes and original plan,
revision, event and Requirements identities accompany the row. It does not infer an issue from a
Task name or execute recovery. For example, against an independently authorized private fixture:

```sh
# needs: an existing native Task/plan in the explicitly selected private Store
python3 -I fixtures/scenarios/results/fleet-document.py --af /private/tools/af \
  --environment fixture --repo /private/project --task-state /private/tasks \
  --task example > fleet-document.json
```

A separately authorized producer can return these exact document bytes as native text or as the
`evidence` of an ordinary observation/action receipt. The generic renderer uses those recorded
bytes; it never runs this script on refresh. Remote capture and recovery permissions belong to
the consumer, not this example. The complete native underlying records remain accessible using
the retained commands and identities.

`authored-markdown.json` exposes the already-authored `authored.md` bytes as its native `report`
text result, plus the source observation Unix milliseconds as named integer result `observed`.
It does not embed `af/result-document@1` JSON. Select the explicit presentation with
`--name report --raw-markdown --observed-output observed --stale-after-ms 60000` on either
`scenario output` or `ant output`; `--watch --interval-seconds 1 --output latest.md` observes it.
The fixed sample observation becomes stale honestly. Like the other static examples, its only
Action is skipped. The executable regression replaces that skipped step with a native human hold,
proves complete ANSI-free atomic refreshes without changing the recorded observation time, then
interrupts the watcher and checks that the original waiting Run and its outputs are unchanged:

```sh
cargo test --locked -p af --test it native_authored_markdown_watch_uses_recorded_observation_and_never_drives_work
```
