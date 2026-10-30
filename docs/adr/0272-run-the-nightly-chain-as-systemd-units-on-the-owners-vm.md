# ADR-0272: Run the nightly chain as systemd units on the owner's VM

**Status:** proposed (2026-10-30).

## Context

The owner's nightly train has four steps, each a script run by hand on the Claw VM: the build
([ADR-0265](0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md)), the two
originals of the nightly executor
([ADR-0266](0266-measure-nightlies-as-originals-of-the-independent-executor.md)), the report
([ADR-0270](0270-compose-the-nightly-report-from-the-nights-record-and-the-executor-journal.md))
and the publication
([ADR-0271](0271-publish-nightlies-as-pre-releases-and-prune-them.md)). Each asked for its night
and its directories on the command line, and nothing submitted the originals or waited for them.
The VM already runs the performance executor under systemd from the installed control
`/opt/af-perf/control`, with its accounts and unit rules
([ADR-0240](0240-execute-performance-experiments-independently-on-a-locked-vm.md)). The chain must
run every night unattended, end every night with a report and a publication whatever the
measurement did, and leave the executor, the release train and GitHub Actions as they are.

## Considered options

1. One orchestrator script that runs the whole night as one process. Rejected: a step that dies
   takes the others with it, and the steps need different accounts (the build and the publish
   hold the nightly key and the write token, the measurement owns the executor's journal).
2. Let the executor's own `af-perf-executor@` timer cycle the nightly configuration. Rejected:
   the nightly step and that timer would race for one journal, and the report could not know
   when the night's originals are done.
3. Let each unit default to today's UTC date. Rejected for the units, kept for the command line:
   the timer fires at 22:05 UTC and the measurement may take five hours, so the report and the
   publish would start after midnight UTC on the next date and find no record. A unit file has
   no date specifier and an `ExecStart` stays one plain Python command, so a unit cannot carry
   the date either.
4. Sequential one-shot units chained by `OnSuccess=` and `OnFailure=`, a measure step that waits
   inside its own unit, and every step's night taken from systemd's record of when the build
   unit started. Chosen.

## Decision

1. **The measure step.** `scripts/nightly/measure.py submit --config nightly.toml` reads
   `builds_root/<date>/nightly.json`. A skipped night measures nothing (exit 0). A built night's
   core and telemetry originals are submitted, through the executor of the tree the script runs
   from, as `cli.py executor --config <[report] executor_config> submit --work core|telemetry
   --kind nightly --tag <tag> --head <head_sha>` (a resubmission prints the existing original),
   then the executor's `status` and `cycle` run in turn until both originals are terminal
   (`complete`, `failed`, `interrupted`, `uncertain`, `expired`) or `[measure] deadline_minutes`
   (30 to 600, default 300) pass. After a `busy` or `idle` cycle, or while another process holds
   the journal, it waits `[measure] poll_seconds` (10 to 600, default 60), never past the
   deadline; no cycle starts after the deadline and none is interrupted. A refused cycle after
   which nothing moved fails the step. Exit 0 when both ended, 1 with `nightly: failed: …` naming
   each original still queued or running, 1 with `nightly: refused: …` when the night has no
   record, the configuration no `[report]` table, or the executor refuses a submission. The
   executor calls go through an injectable runner. The nightly executor's configuration is
   submitted to and cycled by this step alone; the executor, its journal, lock and fence rules
   are unchanged.
2. **The night of a step.** Every step (`build`, `plan`, `measure`, `compose`, `publish`) takes
   `--date YYYY-MM-DD`, or `--night-of UNIT`: the UTC date at which systemd last started the
   service UNIT (`systemctl show --property=ExecMainStartTimestamp --timestamp=unix`, whose
   seconds may be fractional), refused when the unit has not started since boot or last started
   more than 23 hours ago: the chain is then out of order, and no step silently works on a
   later night. Without either the night is today's UTC date from
   the clock. The step prints on stderr the night it chose and why. The four units pass
   `--night-of af-nightly-build.service`, so one chain works on the night its build started, and
   a unit rerun the next morning still works on that night.
3. **The report directories.** `compose --output-root ROOT` writes `ROOT/<date>` when it is free,
   else `ROOT/<date>.<k>` with the smallest free `k` of 2 or more (99 a night at most), each
   tried by an exclusive `mkdir`, and prints the directory it wrote; `publish --report-root ROOT`
   publishes the newest of them, the highest `k`. Without `--output` or `--report` and without a
   root on the command line, both take the root from the configuration's new optional top-level
   key `reports_root`, and print it. `af.nightly-config/1` gains `reports_root` (apart from
   `builds_root` and `mirror`) and an optional `[measure]` table. `af.nightly-config/1` first ships
   with this change, together with its `[report]` and `[publish]` tables: no released `af` or
   script reads an earlier shape of it, so its version stays 1
   ([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)
   versions a shape that shipped).
4. **The units.** `scripts/performance/service/` holds `af-nightly.timer` (`OnCalendar=*-*-*
   22:05:00 UTC`, `Persistent=true`) and four `Type=oneshot` services, each
   `/usr/bin/python3 -I -B /opt/af-perf/control/scripts/nightly/<script>.py … --config
   /etc/af-nightly/nightly.toml --night-of af-nightly-build.service` with
   `Environment=PYTHONDONTWRITEBYTECODE=1`, in the group `af-perf-readers`:
   `af-nightly-build.service` as `af-nightly`, `SuccessExitStatus=3`, `OnSuccess=` the measure;
   `af-nightly-measure.service` as `af-perf-exec`, `OnSuccess=` and `OnFailure=` the report,
   `After=af-perf-source-refresh.service`, `TimeoutStartSec=18h30min` (the longest deadline and
   one cycle of the example's `run_seconds`); `af-nightly-report.service` as `af-perf-exec`,
   `--output-root /var/lib/af-nightly/reports`, `OnSuccess=` the publish only, since a refused
   compose has nothing to publish; `af-nightly-publish.service` as `af-nightly`,
   `--report-root /var/lib/af-nightly/reports`. All carry the executor units' bounds; the
   build's `TimeoutStartSec=2h`, the report's and the publish's `1h`, with the measure's
   `18h30min`, add to 22 h 30 min, less than the day between two timer starts, so a chain never
   overlaps the next build and the 23-hour refusal above never fires in a chain that runs within
   its bounds. The build
   and the measurement keep the host's `/tmp` and mount namespace (`PrivateTmp=no`,
   `PrivateMounts=no`, `OOMPolicy=stop`) because Docker and the machine lock need them; the
   report and the publish are hardened (`ProtectSystem=strict`, `ReadWritePaths=`), the report
   keeping the host's `/tmp` to read whether the machine lock is held.
5. **Secrets.** Tokens and the key's passphrase stay files: never printed, logged, written into a
   unit as a value or passed as an argument. The build reads the passphrase from
   `AF_NIGHTLY_SIGN_PASSWORD` or the systemd credential `af-nightly-sign-password`; the units
   show `LoadCredential=` lines and an `EnvironmentFile=` alternative, commented, naming files
   only.

[`docs/nightly.md`](../nightly.md) is the runbook: install, the first night by hand, what runs
when, amending and recovery.

## Consequences

The owner's VM builds, measures, reports and publishes a nightly every night without a hand on
it, and a night whose measurement failed or ran out of time is still reported and published.
The executor, its journal format, lock and fence rules, `install.sh`, `scripts/release.sh`, the
release workflow, `.af/af.lock` and the hosted performance schedules are unchanged. A refused
build or compose ends that night's chain and shows in `systemctl --failed` and in the next
report's machine section. The `--date` default stays today's UTC date for a step run by hand;
the units never rely on it.

`scripts/performance/tests/test_nightly_measure.py` proves, with a fake executor and clock: the
two submissions with their exact arguments then `status` and `cycle` until both originals end,
a skipped night that runs nothing, a deadline that fails naming what is still open, a
resubmission accepted, a refused cycle, a missing record refused, `--night-of` after midnight,
and the units' accounts, chain, exit status, mounts, timer and absent secrets read as text. The
build, report and publish modules prove the date default, the report roots and the
passphrase credential.
