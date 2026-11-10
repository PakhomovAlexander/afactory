# Documentation

Where to look by what you want to do. The [README](../README.md) covers installing, the quickstart, and the
`.af/` layout; the CLI documents itself with `af help` (every topic), `af help start`,
`af help concepts`, `af help providers`, and `--help` on every namespace and command.

## Try it

`af` with no arguments at a terminal opens a read-first browser over this place's settings,
Providers, Workers, pipelines and Tasks (including Review); `?` there lists its keys and `:` runs
any af command ([design](design/tui.md)).

1. [The README Quickstart](../README.md#quickstart) — install, connect a Provider, onboard a
   repository and review a change, four steps with one command each; `af doctor` checks each.
2. `af help concepts` — Task, Worker, Attempt, Snapshot, Round, Campaign, Store, Provider, Gate,
   Pipeline and Candidate, two sentences each, with the command that shows each one.
3. [`../CONTEXT.md`](../CONTEXT.md) — the whole vocabulary, with the terms each word must not be
   confused with. Everything below assumes it.
4. [`providers.md`](providers.md) — getting model access onto a machine, safely, before running
   anything that spends tokens.
5. [`tasks.md`](tasks.md) — the `af task` walkthrough, once review makes sense.

## Do a job

- [Run a Task](tasks.md), then use the [agent loop](agents.md) to plan, inspect findings,
  verify and deliver it.
- [Connect a Provider](providers.md) before work that spends tokens.
- [Use a Workspace](project.md) to keep Task state with its checkout.
- [Run a report Task](task-execution/report.md) or a
  [Remote Check](task-execution/remote-checks.md).

## Look up a contract

- [Task runtime](task-execution.md), [Task inputs](task-execution/task-inputs.md), and
  [package-defined structured Tasks](task-execution/structured.md).
- [Worker local development](design/worker-local-development.md) and its
  [Rust/Python examples](../fixtures/worker-development/README.md).
- [Experiment Tasks](task-execution/experiments.md), [self management](self-management.md),
  [storage and telemetry](telemetry.md).
- [CLI help](../README.md) for exact commands and `schemas/` for JSON documents.

## Understand the design

- [Architecture](architecture.md) and [accepted decisions](adr/README.md) explain the
  boundaries and tests that enforce them.
- [Vocabulary](../CONTEXT.md), [values](values.md), [non-goals](non-goals.md) and
  [work in progress](design/README.md) explain the reasons behind those boundaries.

## Detailed document index

| Document | Read it when you want to know |
|---|---|
| [`architecture.md`](architecture.md) | How AF is built: crate dependencies, shared contracts, Task execution, Review, Scenario/Project Host boundaries, storage, source capture, checks, Workers, sandboxes, configuration and graph planning. Includes internal naming and protected external names. |
| [`../CONTEXT.md`](../CONTEXT.md) | What a word means. Snapshot, Subject, Base, Campaign, Round, Report, Finding, Attempt, Provider and the rest, each with the nearby term it must not be confused with. Read it before arguing about behaviour. |
| [`providers.md`](providers.md) | How a machine gets model access: finding and registering its logins with `af provider detect`, registering a Provider, why an interactive login is opt-in and terminal-only, what `status` checks versus what `doctor` proves, and every `af provider` exit code. |
| [`tasks.md`](tasks.md) | How to run implementation Tasks with `af task`: start, follow and inspect a run, read its one state and next command (`af help task-states`), resume an interrupted run, retry a failed gate, continue a reviewed Task (`af help task-file`), warm checks, and deliver to a new local worktree. |
| [`agents.md`](agents.md) | The loop an agent or a script drives with `af task`: plan, run or detach, read the state and findings, diff or materialize, run another round, deliver, describe the pull request (`af help agents`). |
| [`project.md`](project.md) | What `af project` knows about a repository: its Project identity and Project Store, its Workspaces (one checkout per line of work, with its own Task Store, Task files and bindings), every `af project` command, `--workspace` on `af task`, and what is written where. |
| [`task-execution/preflight.md`](task-execution/preflight.md) | Offline orchestration checklist for source lineage, declared budgets and evidence ordering; advisory only. |
| [`scenarios.md`](scenarios.md) | Durable Scenarios, signed decisions, bounded adapters, owned-file cleanup, daily Ant scheduling, pinned fresh Tasks and complete-loop roadmap admission. |
| [`ci-health-maintenance.md`](ci-health-maintenance.md) | Hermetic Phase 1 CI observation, diagnosis, native field projection, report-only Scenario routing and the independent bounded watchdog; no live activation. |
| [`scenario-github-adapter.md`](scenario-github-adapter.md) | Optional installed GitHub Scenario adapter: private authority, exact locked-head publication, durable recovery, CI/merge observations and inert examples. |
| [`task-execution.md`](task-execution.md) | The Task runtime reference: fixed product decisions, contracts, fixtures, and walkthroughs for the common Task execution abstraction. |
| [`task-execution/task-inputs.md`](task-execution/task-inputs.md) | The Task file's `inputs` table: binding a root input port to a recorded Task's output instead of exporting it to a file, what resolution records, and how a binding is shown. |
| [`task-execution/experiments.md`](task-execution/experiments.md) | How an experiment Task measures a declared command on the source and on a candidate, compares the two under a declared objective, and gates its evaluator on the result: the `[measures]` and `[objectives]` policy tables, the report line, failure reasons and comparison rules. |
| [`task-execution/remote-checks.md`](task-execution/remote-checks.md) | How a declared check runs as a Remote Check: the policy's `remote` table, the pipeline's `remote_checks`, the operator's machine-local push target, the plan's `publish-gate` effect, the gate branches and draft pull request, the recorded evidence and reasons, cleanup, and what a workflow must allow. |
| [`task-execution/structured.md`](task-execution/structured.md) | Package-defined typed JSON inputs and outputs, bounded capture, deterministic structural acceptance and provenance, normal Worker execution and native Scenario consumption. |
| [`task-execution/report.md`](task-execution/report.md) | How a report Task's author reads an exact source Snapshot, how the kernel resolves the report's repository citations against that Snapshot's Manifest, and how an independent verifier on the same Snapshot accepts it: `report_sources`, `af/DocumentDraft@3`, the report policy, checks and output. |
| [`self-management.md`](self-management.md) | What the installer does and prints, how releases are signed and verified, how af updates itself, its stable, rc and nightly channels and `af self channel`, how it checks for newer releases (when the check runs, the bounds on every network call, what the `af/self-check@4` cache records, how a stalled check is reported, and what `af self status` prints for a stable or a pre-release user), shell completions and man pages, source builds, and removing af with or without `--purge`. |
| [`telemetry.md`](telemetry.md) | What Telemetry records about af's own work and what it never touches: the spans a run records, the four levels, how credentials are withheld, the machine-only `[telemetry]` table, a run's executions and retained history, `af telemetry status`, `reindex`, `runs`, `show`, `tail` and `gc`, telemetry in the Storage Budget, and the layout of the telemetry store. |
| [`performance-ci.md`](performance-ci.md) | How the continuous performance workflow schedules pull request, main, nightly and dispatch runs from the trusted control: the operator's image and runner variables, the owner-review prerequisite, the hosted sample scratch, each job, reruns, and what has not been demonstrated. |
| [`issue-triage-runner.md`](issue-triage-runner.md) | The staged claw-3 triage schedule, private journal and spend reservations, shared nightly lease, first-week dry-run, publication recovery, weekly sweep and explicit operator write transition; hermetic evidence and unperformed deployment. |
| [`nightly.md`](nightly.md) | How the owner's Linux VM builds a signed nightly af from the newest green main, measures it on the nightly executor, composes its daily report and publishes it as a GitHub pre-release, every night as a chain of systemd units: the `af.nightly-config/1` keys, the night's directory, the exit codes, the measure step, the units and when they run, installing them, the first night by hand, amending and recovery, the report's inputs, sections and labels, pruning, the Telegram digest and the `af.nightly-publish/1` receipt ([ADR-0265](adr/0265-build-nightlies-from-the-newest-green-main-on-the-owners-vm.md), [ADR-0271](adr/0271-compose-the-nightly-report-from-the-nights-record-and-the-executor-journal.md), [ADR-0272](adr/0272-publish-nightlies-as-pre-releases-and-prune-them.md), [ADR-0273](adr/0273-run-the-nightly-chain-as-systemd-units-on-the-owners-vm.md)). |
| [`performance-local-executor.md`](performance-local-executor.md) | Independent Claw compute, selectable Actions dispatch, original journals, shared lock fences, local calibration and operator installation/switching. |
| [`performance-cohort-dispatch.md`](performance-cohort-dispatch.md) | The bounded cohort operator, disabled preflight, exact installed bytes, durable dispatch intent and the required new cohort after the repair is committed. |
| [`test-classes.md`](test-classes.md) | Functional/e2e/uix/perf selection, the audited catalog, stable CI gates and commit-bound release profiles. |
| [`test-quarantine-claw.md`](test-quarantine-claw.md) | How to confirm the quarantined tests in 50 runs under load on the Claw VM in a container, apply the report on the owner's machine, file the flaky and P0 issues and open the pull request, and why a macOS-only entry needs a macOS confirmation. |
| [`performance-sdlc.md`](performance-sdlc.md) | The SDLC rollout: PR and release comparisons, image provisioning, the stable check, local evidence collection and the transition from advisory reporting to calibrated enforcement. |
| [`performance-telemetry.md`](performance-telemetry.md) | Telemetry in the performance laboratory: telemetry captures of diagnostic child runs and their supported matrix, the advisory telemetry experiments with their containment, profiles, results, verification and history, the Performance telemetry workflow with its trust limits, the telemetry collection lane, and what is not provided. |
| [`performance-task-settlement-repair.md`](performance-task-settlement-repair.md) | The deterministic accounting-fixture and Review completion regressions, retained state, exact focused commands and limits of the evidence. |
| [`performance-runtime-new-investigation.md`](performance-runtime-new-investigation.md) | The `authority_runtime_new` slowdown: the historical comparison, the phase traces of the current source, the hypotheses kept apart from the facts, the reproduction commands and what was not measured. |
| [`performance-scenarios.md`](performance-scenarios.md) | The bounded native lab adapter, exact accepted Snapshot handoff, research packets, manual Claw proof and later pinned Ant enablement. |
| [`performance-acceptance.md`](performance-acceptance.md) | The performance laboratory's acceptance matrix: what is implemented and tested, what was measured, which environments are unavailable, and the pending multi-day, hosted, staging and operator evidence with each blocker's next step. |
| [`performance-evidence/p8-local/`](performance-evidence/p8-local/README.md) | One real same-source run of the performance workflow on a local macOS arm64 host, retained by package p8's closeout: its commands, exits, identities, bundles, report, trace child, archive, history, reproduction and link and browser checks, and how to reconcile its measured source with the sealed one. Functional evidence, not a speed claim. |
| [`performance-evidence/tui-open-local/`](performance-evidence/tui-open-local/README.md) | The first real local run of the bare-`af` browser case `tui_open`, both registry instances, on a macOS arm64 host (ADR-0263): its steps and exits, every record's `browser_ms` and `splash_ms`, and the earlier attempt whose registry setup could not finish. Functional evidence, not a demonstration. |
| [`performance-archive.md`](performance-archive.md) | How to provision and run the performance laboratory's trusted collector and evidence archive: its credential, commands, schedule, health problems, retention, and what has not been demonstrated. |
| [`non-goals.md`](non-goals.md) | What the kernel deliberately does not do, so a missing feature can be told from a refused one. |
| [`measurements/task-startup-0.12.0-rc.2.md`](measurements/task-startup-0.12.0-rc.2.md) | Where Task startup spent its CPU on 0.12.0-rc.2 with a large captured package: per-phase timings, decode, validation and serialization counts, I/O counters, and what the measurement cannot show. A record, not a decision. |
| [`security/containment-probes.md`](security/containment-probes.md) | What a hostile check is contained by, which probes prove it, and which containment probes are still open. |
| [`values.md`](values.md) | The eight engineering values in priority order and the three product values, each with the tests that decide whether a change honours it. Binding. |
| [`design/`](design/README.md) | The designs still in flight: [warm layers](design/worker-warm-layers.md) with its [package plan](design/worker-warm-layers-plan.md), the [self-optimizer](design/self-optimizer.md), [research pipelines](design/research-pipelines.md), and the [continuous performance laboratory](design/continuous-performance.md). Direction, not shipped behaviour; where a note and an ADR disagree, the ADR wins. |
| [`../brand/`](../brand/README.md) | The design system: the logo files and every asset derived from them, the colour, type and spacing tokens, and the rules for the website, the TUI and GitHub. `brand/gen.py` regenerates the assets. |
| [`adr/`](adr/README.md) | The binding decisions, one per file, with the options that were rejected and why. The stable guide in `adr/README.md` documents discovery with `python3 -I scripts/adr.py list`. |
| [`../CHANGELOG.md`](../CHANGELOG.md) | What each release changed and whether committed `.af/` policy keeps working, needs `af onboard --refresh-lock`, or needs a documented hand edit. |

## Troubleshooting

The messages people meet most, in the words `af` prints them (`…` stands for a path, a name or
a revision), what each means, and what to do. A test checks that `af` still prints every one.

| Message | Meaning | Fix |
|---|---|---|
| `Cannot review an empty Diff Subject` | The selected Base and candidate have no changed paths. Both planning and execution refuse this subject. | Change source bytes, or choose different `--base` and `--candidate` commits; use native Task inspection for advisory exports. |
| `No native Task catalog` | Onboarding or doctor found no current common Task catalog; retired Campaign definitions are not read. | Review a fresh `af onboard` scaffold in a separate checkout, replace the retired authority and commit the native catalog. |
| `Expected an exact recorded plan ID` | An archived Proposal export names a malformed plan digest. | Copy the exact plan ID from `af task show ID --json`, then use `af review export ID PROPOSAL_ID --plan PLAN_ID`. |
| `The requested archived plan has no recorded native execution in this Task` | The selected plan is not an executed plan recorded by this Task. | Inspect a recorded plan with `af review inspect ID --plan PLAN_ID --json`; export only that plan's admitted sealed Proposals. |
| `Finding … has … applicable sealed Proposals; export by exact Proposal ID` | Finding lookup does not identify exactly one applicable repair. | Select an exact Proposal ID from the recorded Review artifacts; deliberate stale export also requires `--allow-stale`. |
| `Provider … not found in this machine's Provider registry` | A Task Provider binding names an ID this machine has not registered. | `af provider status` lists the registry; `af provider setup ID --kind KIND` registers one. |
| `Task … not found in …` | No Task by that ID is in the Task Store the command read. | `af task list` (with the same `--state` or `--workspace`) lists its Tasks. |
| `Task Store not found in …` | The command reads a Task Store that does not exist yet. | `af task plan --file FILE` creates it; check `--state` or `--workspace`. |
| `cannot open Task file …` | The Task file path cannot be opened; the system's reason follows. | Check the path given to `--file`. |
| `is not a Task file field; its fields are` | The Task file names a field `af.task-file/1` does not have, often a misspelling. | Rename or remove the field; the message lists the fields a Task file takes. |
| `must be an integer from` | A Task file value has the wrong shape; the message names its field path and what it holds. | Write the value as the message says, for example a number rather than a string. |
| `--timeout-secs must be positive` | A zero timeout was given; it is refused before any file is read. | Pass a positive number of seconds, or leave the flag out. |
| `limits.wall_ms=0 must be positive` | The Task file's `limits.wall_ms` is 0, which leaves the Task no time to run; the message names the file. | Set `limits.wall_ms` to the Task's wall-clock budget in milliseconds, then run the `af task plan --file …` the message names. |
| `Store … was written by … (this is af …); fix: AF_VERSION=` | A Store was written by another af release, which this one does not replay (ADR-0145). | Run the command under that release with `AF_VERSION=…`, install it with `af self install …`, or move the project pin with `af onboard --refresh-lock --af …`. |
| `was written by an older af release (no version marker` | A Store from a release before Stores named their writer. | Finish its work under the release that started it, or remove it with `af storage prune --apply` once idle. |
| `Stores written by other releases` | A listing met several Stores other releases wrote: one row when one release wrote them all, else one line per release with its own command. | Run each with the `AF_VERSION=…` command of its own release, or `af storage --verbose` to see each. |
| `could not be measured and` | `af storage` met paths it could not measure, such as a half-made Store a test registered. | `af storage --verbose` names each path and why. |
| `Output port … not found in the result of Task` | `af task output --port` names a port the Task's result does not have; the message lists the ones it has. | Pass one of the ports listed. |
| `Proposal export writes a patch; omit --json` | Patch export is requested with JSON output. | Omit `--json`; inspection commands provide recorded JSON, while export writes the verified sealed patch. |
| `Spotlight indexes …, which every build and sandbox writes to` | `af doctor` on macOS found Spotlight indexing a root af's builds and sandboxes write to. | Exclude it in Spotlight's Search Privacy, or move it to a directory named `*.noindex` (below). |

### macOS: Spotlight and Gatekeeper

Every gate on macOS builds into af's warm build cache (`[storage] build_cache_root`, by default
`~/.cache/af/task-build-cache`), its warm Workspaces (`~/.cache/af/workspaces`) and its
sandboxes (`[storage] sandbox_root`, by default `$TMPDIR`). Spotlight indexes every file written
there and Gatekeeper scans every new binary, which one machine measured at four to five cores
(`mds`, `mdworker` and `syspolicyd`) for the length of a gate. `af doctor` warns when Spotlight
indexes one of these roots. Exclude them once:

- add each root `af doctor` names in System Settings > Spotlight > Search Privacy; or
- point the roots at directories whose names end in `.noindex`, which Spotlight skips, for
  example `AF_STORAGE__BUILD_CACHE_ROOT=~/Library/Caches/af-build.noindex` and
  `AF_STORAGE__SANDBOX_ROOT=~/Library/Caches/af-sandboxes.noindex`, or the same keys under
  `[storage]` in `~/.config/af/config.toml`;
- and add your terminal under System Settings > Privacy & Security > Developer Tools
  (`spctl developer-mode enable-terminal` makes the entry appear), so Gatekeeper does not scan
  each binary a build writes.

## Reading and writing ADRs

Follow the stable [ADR authoring and lifecycle guide](adr/README.md). Add only the decision's
own `YYYY-MM-DD-descriptive-topic.md` file, with a unique topic and stable date-topic identity;
no sequence allocation or central index edit is needed. Open with status and date, state the
context, rejected options and reasons, decision and consequences. Accepted identities and
bodies stay immutable; supersession follows the guide's status-only and deleted-link rules.
Legacy numbered identities and historical exceptions remain unchanged.

Browse the [decision directory on GitHub](https://github.com/PakhomovAlexander/afactory/tree/main/docs/adr)
or run `python3 -I scripts/adr.py list` from the repository root for deterministic titles,
recorded statuses and relative file links. Output is derived on demand and never committed.

## Conventions

- The words in `CONTEXT.md` are used with their defined meaning everywhere else. If a document
  needs a new term, define it there first.
- A claim about behaviour cites the test or fixture that pins it; a claim about a boundary cites
  the ADR that fixed it.
- Project-specific pipelines, Worker packages, and Campaign state belong in the consuming
  repository, not here. This repository's own `.af/` is an example that a test asserts still loads.
