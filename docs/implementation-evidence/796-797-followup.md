# Issues 796/797: bounded predecessor correction

Implementation evidence on 2026-11-11; this is not independent acceptance or a native full-gate
result. The captured predecessor feature implementation is retained. No Rust implementation,
fixture, schema, assertion, test classification, gate, budget or sandbox boundary was changed.

## Candidate changes

Deleted only `.af/task-packages/owner-impl`, its six package declarations and four provider
bindings in `.af/task-catalog.toml`. A parsed TOML before/after comparison confirmed all other
catalog content is equal. The separately captured authority executing this Task was not edited.
The package-pin audit passes. No personal execution configuration was added.

Quoted the multiplication expression on ADR line 28 and demoted the raw evidence section headings
from H1 to H2. Historical evidence remains intact, including the original failed fast gate,
process-liveness failures, explicit `Operation not permitted` diagnostics and interrupted UIX
checks. Historical notes now link here. The storage fixture already contains `kind: daily`;
it required no additional edit. No new behavior/contract decision was needed; the predecessor's
proposed dated ADR and changelog remain, with a changelog note for the authority correction.

## Reproduction environment

Darwin/arm64, Rust/cargo 1.88.0, offline locked dependencies, debug build in checkout `target/`.
Only registry/git caches were privately copied into `target/followup/cargo-home`; no credentials
or host machine configuration were copied. Commands below use this environment from repository root:

```sh
export PATH="/Users/apakhomov/.rustup/toolchains/1.88.0-aarch64-apple-darwin/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export CARGO_HOME="$PWD/target/followup/cargo-home"
export HOME="$PWD/target/followup/home" TMPDIR="$PWD/target/followup/tmp"
export XDG_CONFIG_HOME="$HOME/config" XDG_STATE_HOME="$HOME/state"
export XDG_CACHE_HOME="$HOME/cache" XDG_DATA_HOME="$HOME/data" XDG_BIN_HOME="$HOME/bin"
export CARGO_NET_OFFLINE=true
```

Private writable directories were created before execution. Integration tests additionally use
`crate::common::af()` isolation. No production Ant, recovery monitor, service activation,
publication, external message or machine configuration change occurred.

## Fresh focused checks

Each Rust command below begins with `cargo test --offline --locked`. Times are test-harness elapsed
seconds, excluding build and lock waits. No full fast gate was duplicated in the implementer role.

| Remaining command arguments | Exit | Result | Seconds |
| --- | ---: | --- | ---: |
| `-p af --test it catalog_digest::every_` | 0 | 4 passed, including both previously failing caller-authority audits, package pins and rubric | 2.94 |
| `-p af --test it action_only_scenario_start_resume_and_ant_tick_install_and_sweep_the_storage_budget` | 0 | 1 passed, original storage-budget assertions | 1.96 |
| `-p af-runtime --test it task_review_execute_checks::a_container_check_executes_on_the_native_task_host -- --ignored --exact --nocapture` | 0 | 1 passed, unchanged live check assertions | 1.54 |
| `-p af-scenario --lib` | 0 | 61 passed, including interval/daily/replay/output contracts | 0.34 |
| `-p af --test it scenario::interval_scheduler_executes_real_one_second_occurrences_and_generic_results -- --exact` | 0 | 1 passed | 6.58 |
| `-p af --test it scenario::markdown_watch_interrupt_is_read_only_atomic_and_retains_active_run -- --exact` | 0 | 1 passed | 2.88 |
| `-p af --test it scenario::recorded_observation_results_are_partial_then_final_and_new_occurrences_change_revision -- --exact` | 0 | 1 passed | 4.27 |
| `-p af --test it scenario::scheduler_fatal_journal_failure_stops_and_keeps_run_and_diagnostic -- --exact` | 0 | 1 passed | 1.86 |
| `-p af --test it scenario::fleet_result_example_projects_native_goal_state_clock_and_charged_usage -- --exact` | 0 | 1 passed | 2.21 |

The first container-probe invocation omitted `/usr/local/bin` from PATH and **failed** (cargo exit
101, 0 passed/1 failed, 0.70s), at the unchanged assertion requiring a usable runtime: Podman was
installed but could not connect. Inspection found Docker at `/usr/local/bin/docker`. Adding that
existing executable directory to the private test PATH and rerunning the exact command above
passed, including successful, failed and mutated native check paths. No daemon was installed,
started or reconfigured. This failed setup result is retained; it is not called flaky and does
not explain or replace the predecessor native gate's result.

Markdown checks use the existing configuration and cached `markdownlint-cli2@0.22.1`:

```sh
node /Users/apakhomov/.npm/_npx/38d773fe1452947e/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs \
  docs/adr/2026-11-11-native-intervals-and-recorded-result-markdown.md \
  'docs/implementation-evidence/796-797*.md'
python3 -I scripts/changelog-notes.py --check
```

The final focused Markdown command above exits 0 (five files, zero errors); the changelog
check exits 0.

An exploratory `**/*.md` lint after cache copying also traversed dependency READMEs in the ignored
build directory and exited 1. Source-only Markdown enumeration with `rg --files --hidden -g '*.md'
-g '!target/**' -g '!.git/**'` passed (490 source files before this new note, zero errors).
This is a check of source inputs; the native Markdown gate/configuration was not edited.

## Private repeated execution and measurement

```sh
python3 -I scripts/measure-ant-intervals.py --af target/debug/af --seconds 5
```

Exit 0. Binary SHA-256: `f052b911898cec5c1dff180c6ae568b6cd7f43c66583c44c0be3baff467f7a8b`.
Weather and library each started four unique successful zero-dispatch Runs, with zero model-token
allowance, and rendered both named `report` and `empty` outputs. The unrelated author-defined views
include no Task/issue columns. The independent 1s watcher ran after the scheduler stopped.

| Mode | Child exit | Wall seconds | User seconds | System seconds | CPU, one core | Package/interrupt wakeups | Sample seconds | Journal growth bytes |
| --- | ---: | ---: | ---: | ---: | ---: | --- | ---: | ---: |
| idle | 0 | 5.073510 | 0.025076 | 0.018462 | 0.8581% | 0/4 | 4.767935 | +4096 |
| one_second | 0 | 5.066251 | 0.128235 | 0.030802 | 3.1391% | 0/4 | 4.753766 | +94208 |
| watch_one_second | -2 | 5.021122 | 0.008643 | 0.008194 | 0.3353% | 0/4 | 4.759769 | +0 |

The watch was intentionally interrupted by SIGINT (`wait4` -2, shell 130), leaving a valid atomic
Markdown file. CPU uses per-owned-child `wait4` accounting; wall time is monotonic. Wakeups use
macOS `proc_pid_rusage` v2 sampled before exit, not physical host wakeups. Journal growth uses
logical SQLite sizes. Idle means two registered disabled private fixture Ants. These five-second
debug samples ran alongside focused compilation/tests, not on an isolated benchmark host;
no release-comparison, production CPU or hard real-time precision claim follows.
The native 16 MiB journal limit and machine storage accounting remain unchanged; high-frequency
retained Run growth still limits duration. Watching added no journal bytes.

Selected first/last native started receipts (all identities were checked unique by the recipe):

| Ant | Index | Due Unix ms | Admitted Unix ms | Lateness ms | Occurrence identity |
| --- | ---: | ---: | ---: | ---: | --- |
| weather | 6 | 1794412949880 | 1794412950068 | 188 | `sha256:b3495a05203829f0399eb35a7b95f08cf0c6aaee8ab9be3da5413b8202be9b43` |
| weather | 9 | 1794412952880 | 1794412953100 | 220 | `sha256:544df4d038d746cdd07cc300deb72aa3269136ce383759533b2b0f6d9f3a1ca7` |
| library | 6 | 1794412949921 | 1794412950068 | 147 | `sha256:2dd6c9a78d6e87ba85c6bb44fe5bf0357984fa5a9f3af18b15c4598afa59df21` |
| library | 9 | 1794412952921 | 1794412953100 | 179 | `sha256:1aec1fb7fcf7d8e47c88c499db4ea8735c732cb5c71a485ea3fd2aa54090f814` |

## Acceptance checklist and limits

| Item | Implementer assessment |
| --- | --- |
| interval-scheduler | Retained implementation; fresh 61-case domain suite covers 1/5/60/900s, bounds, clocks, overlap, restart/replay and leases. Real-process repeated 1s cases and short overhead measurements pass. |
| generic-markdown | Retained implementation; arbitrary weather/library outputs and fleet projection execute; domain validation and named-output/revision CLI cases pass. |
| read-only-watch | Fresh interruption/atomic-file/active-Run and partial/final/revision/error cases pass; independent 1s watch measurement adds zero journal bytes. |
| integrity-tests-docs | Caller-only product authority removed; catalog pins/audits, storage and live container probe pass with unchanged assertions. Prose lint corrected; prior failures preserved. |

Whole-Task acceptance remains **pending** the unchanged native local fast gate and independent
correctness build/stage/execution `ImplementationTestEvidence` on the sealed candidate. Those results
are unavailable in this implementation Attempt. The predecessor full-gate, process/telemetry failures
and UIX interruption remain historical failures; these focused passes do not turn them into passes.
No independent-review evidence is authored or impersonated here. Telegram remains out of scope.
No external issue was filed under the explicit prohibition on external messages and credentials.
