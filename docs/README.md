# Documentation

Where to look by what you want to do. The [README](../README.md) covers installing, the quickstart, and the
`.af/` layout; the CLI documents itself with `af help` (every topic), `af help start`,
`af help concepts`, `af help providers`, and `--help` on every namespace and command.

## Try it

`af` with no arguments at a terminal opens a read-first browser over this place's settings,
Providers, Workers, pipelines, Tasks and review Campaigns; `?` there lists its keys and `:` runs
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

- [Task runtime](task-execution.md) and [Task inputs](task-execution/task-inputs.md).
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
| [`architecture.md`](architecture.md) | How the kernel is built: crates, contracts, the store, source capture, Check nodes, reviewer adapters, sandboxes, the pipeline graph, pipeline definitions, routing, and Attempt budgets. Each section names the test that pins it. |
| [`../CONTEXT.md`](../CONTEXT.md) | What a word means. Snapshot, Subject, Base, Campaign, Round, Report, Finding, Attempt, Provider and the rest, each with the nearby term it must not be confused with. Read it before arguing about behaviour. |
| [`providers.md`](providers.md) | How a machine gets model access: finding and registering its logins with `af provider detect`, registering a Provider, why an interactive login is opt-in and terminal-only, what `status` checks versus what `doctor` proves, and every `af provider` exit code. |
| [`tasks.md`](tasks.md) | How to run implementation Tasks with `af task`: start, follow and inspect a run, read its one state and next command (`af help task-states`), resume an interrupted run, retry a failed gate, continue a reviewed Task (`af help task-file`), warm checks, and deliver to a new local worktree. |
| [`agents.md`](agents.md) | The loop an agent or a script drives with `af task`: plan, run or detach, read the state and findings, diff or materialize, run another round, deliver, describe the pull request (`af help agents`). |
| [`project.md`](project.md) | What `af project` knows about a repository: its Project identity and Project Store, its Workspaces (one checkout per line of work, with its own Task Store, Task files and bindings), every `af project` command, `--workspace` on `af task`, and what is written where. |
| [`task-execution/preflight.md`](task-execution/preflight.md) | Offline orchestration checklist for source lineage, declared budgets and evidence ordering; advisory only. |
| [`task-execution.md`](task-execution.md) | The Task runtime reference: fixed product decisions, contracts, fixtures, and walkthroughs for the common Task execution abstraction. |
| [`task-execution/task-inputs.md`](task-execution/task-inputs.md) | The Task file's `inputs` table: binding a root input port to a recorded Task's output instead of exporting it to a file, what resolution records, and how a binding is shown. |
| [`task-execution/experiments.md`](task-execution/experiments.md) | How an experiment Task measures a declared command on the source and on a candidate, compares the two under a declared objective, and gates its evaluator on the result: the `[measures]` and `[objectives]` policy tables, the report line, failure reasons and comparison rules. |
| [`task-execution/remote-checks.md`](task-execution/remote-checks.md) | How a declared check runs as a Remote Check: the policy's `remote` table, the pipeline's `remote_checks`, the operator's machine-local push target, the plan's `publish-gate` effect, the gate branches and draft pull request, the recorded evidence and reasons, cleanup, and what a workflow must allow. |
| [`task-execution/report.md`](task-execution/report.md) | How a report Task's author reads an exact source Snapshot, how the kernel resolves the report's repository citations against that Snapshot's Manifest, and how an independent verifier on the same Snapshot accepts it: `report_sources`, `af/DocumentDraft@2`, the report policy, checks and output. |
| [`self-management.md`](self-management.md) | What the installer does and prints, how releases are signed and verified, how af updates itself, its stable, rc and nightly channels and `af self channel`, how it checks for newer releases (when the check runs, the bounds on every network call, what the `af/self-check@4` cache records, how a stalled check is reported, and what `af self status` prints for a stable or a pre-release user), shell completions and man pages, source builds, and removing af with or without `--purge`. |
| [`telemetry.md`](telemetry.md) | What Telemetry records about af's own work and what it never touches: the spans a run records, the four levels, how credentials are withheld, the machine-only `[telemetry]` table, a run's executions and retained history, `af telemetry status`, `reindex`, `runs`, `show`, `tail` and `gc`, telemetry in the Storage Budget, and the layout of the telemetry store. |
| [`performance-ci.md`](performance-ci.md) | How the continuous performance workflow schedules pull request, main, nightly and dispatch runs from the trusted control: the operator's image and runner variables, the owner-review prerequisite, the hosted sample scratch, each job, reruns, and what has not been demonstrated. |
| [`performance-local-executor.md`](performance-local-executor.md) | Independent Claw compute, selectable Actions dispatch, original journals, shared lock fences, local calibration and operator installation/switching. |
| [`performance-cohort-dispatch.md`](performance-cohort-dispatch.md) | The bounded cohort operator, disabled preflight, exact installed bytes, durable dispatch intent and the required new cohort after the repair is committed. |
| [`test-classes.md`](test-classes.md) | Functional/e2e/uix/perf selection, the audited catalog, stable CI gates and commit-bound release profiles. |
| [`test-quarantine-claw.md`](test-quarantine-claw.md) | How to confirm the quarantined tests in 50 runs under load on the Claw VM in a container, apply the report on the owner's machine, file the flaky and P0 issues and open the pull request, and why a macOS-only entry needs a macOS confirmation. |
| [`performance-sdlc.md`](performance-sdlc.md) | The SDLC rollout: PR and release comparisons, image provisioning, the stable check, local evidence collection and the transition from advisory reporting to calibrated enforcement. |
| [`performance-telemetry.md`](performance-telemetry.md) | Telemetry in the performance laboratory: telemetry captures of diagnostic child runs and their supported matrix, the advisory telemetry experiments with their containment, profiles, results, verification and history, the Performance telemetry workflow with its trust limits, the telemetry collection lane, and what is not provided. |
| [`performance-task-settlement-repair.md`](performance-task-settlement-repair.md) | The deterministic accounting-fixture and Review completion regressions, retained state, exact focused commands and limits of the evidence. |
| [`performance-runtime-new-investigation.md`](performance-runtime-new-investigation.md) | The `authority_runtime_new` slowdown: the historical comparison, the phase traces of the current source, the hypotheses kept apart from the facts, the reproduction commands and what was not measured. |
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
| [`adr/`](adr/README.md) | The binding decisions, one per file, with the options that were rejected and why. The index in `adr/README.md` lists every one. |
| [`../CHANGELOG.md`](../CHANGELOG.md) | What each release changed and whether committed `.af/` policy keeps working, needs `af onboard --refresh-lock`, or needs a documented hand edit. |

## Troubleshooting

The messages people meet most, in the words `af` prints them (`…` stands for a path, a name or
a revision), what each means, and what to do. A test checks that `af` still prints every one.

| Message | Meaning | Fix |
|---|---|---|
| `af review plan needs --policy-rev REV, the commit whose committed .af/ governs the review` | A review plan was asked for without saying which commit's `.af/` policy to use. | `af review plan --policy-rev HEAD --base HEAD --uncommitted` |
| `this repository has no .af/ authority at …` | The policy revision holds no `.af/`: the repository was never onboarded, or the policy is not committed yet. | `af onboard`, then `af onboard --apply`, commit `.af/`, and plan again. |
| `policy revision … is not a git revision of …` | Git does not know the name given to `--policy-rev`; its own line follows. | Name a commit, branch or tag of that repository, usually `HEAD`. |
| `-> MISSING; fix: …` | A model Worker of the pipeline has no `--provider NODE=ID` binding; the plan names one to use or how to register one. | Add the `--provider` the line names, or `af provider setup ID --kind KIND --login` first. |
| `EMPTY — run will refuse` | The diff between the base and the candidate changes no file, and `af review run` refuses an empty diff. | Change files and pass `--uncommitted`, or name other revisions with `--base` and `--candidate`. |
| `Provider … not found in this machine's Provider registry` | A `--provider NODE=ID` names an ID this machine has not registered. | `af provider status` lists the registry; `af provider setup ID --kind KIND` registers one. |
| `Node … not found in …` | A `--provider NODE=ID` names a node that is not a model Worker of the pipeline; the message lists the ones it has. | Bind one of the model Workers listed. |
| `model-backed Worker … requires explicit` | `af review run` was started without a binding for a model Worker. | Add `--provider NODE=ID` for every model Worker `af review plan` lists. |
| `Task … not found in …` | No Task by that ID is in the Task Store the command read. | `af task list` (with the same `--state` or `--workspace`) lists its Tasks. |
| `Task Store not found in …` | The command reads a Task Store that does not exist yet. | `af task plan --file FILE` creates it; check `--state` or `--workspace`. |
| `Campaign … not found in …` | No review Campaign by that name exists on this machine. | `af review campaigns` lists them; `af review run --campaign NAME` starts one. |
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
| `is not a command; the browser is` | `af review tui` is not a command. | Run `af` alone at a terminal to open the browser. |
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

An ADR is numbered (`NNNN-kebab-case-title.md`, the next number after the highest in
`docs/adr/`), opens with a status line that names the status and date, states the context, lists
the considered options with the reasons each was rejected, records the decision, and ends with
its consequences. Accepted ADRs are immutable; a change is a new ADR that says what it
supersedes. A partially superseded ADR gains a status-line note linking the new one, and that
status line may be restated when the new ADR spends the transition wording it carried; a fully
superseded ADR is deleted, and git history keeps it, unless the superseding ADR keeps it with a
superseded status line.
Links to a deleted ADR, or to an internal record or completed design note that was deleted, are
rewritten to point at the superseding ADR or to plain text; this is the only edit allowed in
another accepted ADR's body (ADR-0113 clauses 6 and 8, ADR-0145 clause 8). Propose one by copying the shape of a recent ADR, adding it to the
index in [`adr/README.md`](adr/README.md), and opening a pull request.

## Conventions

- The words in `CONTEXT.md` are used with their defined meaning everywhere else. If a document
  needs a new term, define it there first.
- A claim about behaviour cites the test or fixture that pins it; a claim about a boundary cites
  the ADR that fixed it.
- Project-specific pipelines, Worker packages, and Campaign state belong in the consuming
  repository, not here. This repository's own `.af/` is an example that a test asserts still loads.
