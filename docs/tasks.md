# Implementation Tasks with `af task`

`af task` runs one bounded implementation Task against a local Git repository and, when the
result is verified, delivers it to a new local branch and worktree for a human to inspect and
commit. A Task starts from a Task file that names its ID, kind, goal and limits; the Pipeline and
Worker packages that run it are pinned in the repository's committed Task catalog. This page
covers the `implement` Task and its delivery. Embedded Review, generated plans, repair and the
other Task kinds are described under [Task execution](task-execution.md).

## Supported boundary

An implement Task runs its captured Pipeline over a captured source Snapshot. An implementation
Pipeline such as the starter `builtin/implementation-small` runs:

1. an implementer Worker edits a private sandbox, and the result is sealed as a derived Snapshot;
2. the required read-only checks of the repository's code policy inspect that Snapshot;
3. an independent evaluator verifies it against the Task's requirements; it receives only its
   declared inputs, never the implementer's transcript;
4. a verified result remains an immutable Snapshot until an operator explicitly delivers it;
5. delivery creates a new local branch and linked worktree containing those exact bytes as
   uncommitted work.

Delivery never changes the source checkout, commits, pushes, opens a pull request, invokes a
remote, or receives repository credentials
([ADR-0031](adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)). Hosted execution and
automatic Integration are outside this path.

## Prerequisites

- a local Git clone with no staged, tracked or untracked changes at the commit used to start
  the Task;
- an installed `af` release whose archive digest and `SHA256SUMS.minisig` signature were both
  verified. The release's `install.sh` always checks the digest against `SHA256SUMS`, and the
  signature with `minisign`: put it on `PATH` before you run the installer
  ([install](../README.md#install)), which refuses without it. Under
  `AF_INSTALL_ALLOW_UNSIGNED=1` it proceeds anyway, reports that only the checksum is verified,
  and the install receipt records `verified_by = "sha256sums"` rather than `"minisign"`; that
  checksum-only install does not meet this prerequisite. The running release's
  `receipt.verified_by` is shown by `af self status --json`;
- machine-local Provider authentication for every model Worker the Pipeline binds, checked with
  `af provider status` and established as [`providers.md`](providers.md) describes — an
  interactive login is opt-in and happens only at your own terminal; command Workers need none;
- every tool named by the code policy's checks, such as the repository's compiler and test
  runner;
- a reviewed Task catalog committed in the repository: `.af/task-catalog.toml`, the Pipeline and
  Worker packages it pins, and `.af/code-policy.toml` with the required checks.

`af catalog init --profile software --destination task-demo` creates a new starter directory,
which must not exist yet, holding a working, credential-free catalog and runnable Task files
([starters](task-execution/starters.md)); it does not add a catalog to the current repository.
Its `--destination`, like that of `af catalog sync`, is a safe path relative to `--repo`
(default `.`); an absolute path is refused.
`af catalog sync` imports shared definitions from Git
([shared catalogs](task-execution/shared-catalogs.md)). The catalog pins every package by digest,
so changing any package byte requires a newly reviewed pin; `af catalog digest PACKAGE_DIR`
prints that digest ([pin a package](task-execution/shared-catalogs.md#pin-a-package)). Checks
are project-specific, so the code policy is written per repository.

Trusting that committed authority and running `af task start --execute`, or confirming the
previewed plan, authorizes Afactory to deliver each Worker its exact declared inputs for every
stage of the Task
([ADR-0033](adr/0033-configured-workers-authorize-declared-input-delivery.md)). There is no
additional per-Worker or per-call consent prompt. Delivery, Provider rebinding and any other
remote side effect remain separate explicit operations.

## Write a Task file

A Task file is JSON or TOML in the [`af.task-file/1`](../schemas/task-file-v1.json) shape. The
checked-in `fixtures/task-runtime/pagination` example uses this one:

```json
{
  "schema": "af.task-file/1",
  "task_id": "pagination-cli",
  "kind": "implement",
  "goal": "Implement this Jira ticket: offset/limit pagination",
  "pipeline": { "name": "fixture/implementation", "fallback": "refuse" },
  "strategy": "small",
  "facts": {},
  "limits": {
    "tokens": 1000,
    "max_attempts": 3,
    "wall_ms": 60000,
    "verification": { "tokens": 200, "attempts": 2, "wall_ms": 10000 }
  }
}
```

The Task ID is chosen here: planning the same file again shows the existing plan, and a changed
file under a planned ID needs `af task plan --replace`. A file that omits `strategy` takes its
kind's: `implement` is `reviewed` with `verification` `review`, `verification` is `light` with
`evaluation`, both with `facts.standard = true`, and `report` is `report`; a field the file names
overrides the kind's, and one its kind's profile cannot verify by is refused with both values and
this table. Without `pipeline`, planning
[selects](task-execution/selection.md) a fitting Pipeline from the catalog. The goal and any
`requirements` are input data; they cannot change the committed execution permissions. The
[Task-file walkthrough](task-execution/task-file.md) runs this example end to end.

## Start and inspect a Task

Run from the clean repository at committed `HEAD`. State defaults to a directory outside the
repository under the user's XDG state directory; `--state` selects another external directory,
which must be absent, empty or already an af Store: it becomes af-owned, so a directory holding
anything else is refused. `--workspace LABEL` selects a Workspace's Store and bindings file
([Projects and Workspaces](project.md)).
Save the Task request outside the checkout too (for example, `../ticket.json`, or a Workspace's
`tasks/` directory); Afactory captures its contents into Task state. `--file` selects the request,
not the source repository, so remain in the source checkout or select it with `--repo`.

```sh
af provider status
af task start --file ../ticket.json
# Review the preview and copy its complete Plan identity.
af task run pagination-cli --confirm-plan PLAN_ID
af task list
af task show pagination-cli
```

Before the plan, `af task plan` prints one line per Model Worker on stderr, `model WORKER MODEL
EFFORT` followed by `ok` (the Provider's catalog lists the exact ID and the effort level),
`unverified against the catalog` (a custom ID that passes the spelling rule, or a catalog the
harness could not give) or `refused:` and the hint, which refuses the plan. The catalog is read
token-free, once per Provider; a binding to an alias or an effort level the Provider does not
accept is refused before any Provider process starts. `af provider models <id>` lists the
spellings a Provider accepts ([model bindings](task-execution/model-bindings.md),
[ADR-0148](adr/0148-discover-provider-models-and-validate-every-binding-before-a-charge.md)).

The [ASCII preview guide](task-execution/preview.md) covers the two plan views and agent
approval. `task list` prints a header row and one line per Task: its ID, its state, its review
round, chargeable tokens, how long ago it last changed and the verb of its next command, then its
delivery once there is one. `task show` adds the plan, the checks, each failed node and the next
command; `--json` adds exact Snapshot IDs and the append-only event history. Both accept `--json`
([Task states](#task-states)).

## Follow a run

While `af task run` waits it prints on stderr one line per node that starts or finishes, with the
time since the run started and the tokens charged so far (`af task run: implement finished (2 min
elapsed, 41k tokens so far)`), and every 5 minutes a heartbeat (`af task run: implement running 23
min, 412k tokens, host load 11.2`); `--quiet` keeps them back. stderr's last line is always the
Task's state and next command, `Task ID: phase/outcome/cause; next: COMMAND`, in text as under
`--json`.

`af task run ID --detach` runs the same command in the background and returns once that run holds
the Task and its plan is admitted, printing its pid, its host and its log (`af/task-detached@1`
under `--json`). The run has no terminal, survives the shell that started it, and writes its
output to `runs/ID-N.log` in the Task Store; every run, in a terminal or not, also records its
process there as `runs/ID-N.json` before it takes the Task
([ADR-0202](adr/0202-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)).

```sh
af task run pagination-cli --confirm-plan PLAN_ID --detach
af task status pagination-cli
af task wait pagination-cli --timeout 3600
af task cancel pagination-cli
```

`af task status ID` prints one line at once: the state, the node whose Attempt runs, the time since
the Task's clock started, the tokens so far and the run that holds it. `af task wait ID` blocks
until the Task settles and exits with its exit code, or with 124 when `--timeout S` passes first;
both print `af/task-status@1` under `--json`. `af task cancel ID` stops the `af task run` the Task's
writer lease names: it finds the process by the lease's writer in the run records, refuses one of
another host with its pid and host, records who asked in `runs/ID-N.cancel.json`, and signals that
recorded pid only, which stops its Worker processes as Ctrl-C does. The Task is then
`abandoned/cancelled/host_cancelled`, `af task show` names who cancelled it, and `af task run ID`
resumes it. [docs/agents.md](agents.md) walks the whole loop as an agent runs it.

## Task states

Every `af task` command that shows a Task prints one state for it, in text and under `--json`
alike: `af task list`, `af task show`, `af task report` and the last line of `af task run`. The
state is `phase/outcome/cause`, for example `finished/gate_failed/check_failed`; `outcome` is
present once the Task ended and `cause` once that outcome is not `verified`. Under `--json` it is
the `state` object: `phase`, `outcome`, `cause` and `next_action`, `null` where absent
(`af help task-states` prints the same tables).

```text
  planned --af task run--> running --> finished
     |                      |   ^
     |                      v   | af task run (resume)
     |                   abandoned
     +--> waiting (plan review, resources, input or a decision) --> running
```

| Phase | Meaning |
| --- | --- |
| `planned` | a plan is recorded and waits for `af task run` to confirm it |
| `running` | a writer holds the Task's lease and runs it now |
| `waiting` | the Task waits for a plan review, resources, input or a decision |
| `abandoned` | the Task stopped before it finished and no writer holds it |
| `finished` | the Task recorded its result (or was collected after it did) |

| Outcome | Exit code of `af task run` | Meaning |
| --- | ---: | --- |
| `verified` | 0 | every acceptance obligation is satisfied; the result can be delivered |
| `changes_requested` | 3 | the review or the evaluator ran to completion and asked for changes |
| `gate_failed` | 5 | a required check of the gate failed or could not run |
| `provider_failed` | 6 | a Worker's Provider or process failed, or its reply was not admissible |
| `cancelled` | 4 | the run stopped before it finished; `af task run` resumes it |
| `exhausted` | 4 | a token, Attempt or verification-reserve limit ran out |
| `expired` | 7 | the Task's wall clock ran out |

| Cause | Outcome | Meaning |
| --- | --- | --- |
| `reviewer_findings` | `changes_requested` | the review recorded findings at or above its gate |
| `evaluator_rejected` | `changes_requested` | the independent evaluator did not accept the result |
| `check_failed` | `gate_failed` | a required check ran and failed |
| `check_flaky_then_failed` | `gate_failed` | a required check failed; some of its tests were flaky before |
| `preflight_failed` | `gate_failed` | a required check stopped before its tests ran, or never started |
| `malformed_reply` | `provider_failed` | a Worker's process failed or its reply broke its output contract |
| `provider_network` | `provider_failed` | a request to the Provider failed on the network |
| `provider_capacity` | `provider_failed` | the Provider's model was at capacity or its rate limit was reached |
| `provider_timeout` | `provider_failed` | a Worker or Provider call ran out of its Attempt time |
| `identity_changed` | `provider_failed` | the Provider answered as another account, model or policy than the plan bound |
| `identity_unverified` | `provider_failed` | the Provider's login could not be verified |
| `model_refused` | `provider_failed` | the bound model refused, was switched or is unknown to its Provider |
| `writer_lost` | `cancelled` | the writer lost its lease before the Task finished |
| `deadline` | `expired` | the Task's wall clock ran out |
| `budget` | `exhausted` | a token, Attempt or verification-reserve limit ran out |
| `collected` | the one it retained | the Task was collected; only its summary remains |
| `host_cancelled` | `cancelled` | the run was interrupted (Ctrl-C or a signal) |

Every state that ended names the one command to run next: the last line of `af task show` and
`af task run` (`next: af task run ID --retry-gate`), `none` when there is none, and
`state.next_action` under `--json`. It is `af task findings ID` after reviewer findings,
`af task run ID --retry-gate` after a failed gate that the kernel can run again on the same
candidate ([Retry a failed gate](#retry-a-failed-gate)), whatever its cause, `af task show ID
--checks` after one it cannot, `af task run ID` to resume a cancelled run, `af task plan --file
TASK_FILE --fit` after a budget or deadline ran out (with a new `task_id` once the Task ran), and
`af provider status` or `af provider setup ID --login` after a Provider failure. `af task show` prints, per
failed node, its cause, the first line of its error and the artifact IDs of its whole diagnostic
and stderr; a node that only lacks the receipt of a branch that failed comes after that branch.
A command that exits with an outcome's code while its result goes to stdout, such as `af task
explain` or `af task run` of a Task that already finished, ends with the same line on stderr as
the run log does, `Task ID: finished/gate_failed/check_failed; next: af task run ID --retry-gate`:
a non-zero exit always says why on stderr.

`af task show ID --checks` prints the gate's last check receipt: each check's status, duration,
warm or cold directories, the step that stopped it, its failing tests (named from the runner's
`FAIL`, `TIMEOUT` and `SIGABRT` lines), its `flaky:` tests and the last 40 lines of its stderr;
`--json` prints one [`af/task-checks@1`](../schemas/task-checks-v1.json) document. `af task list
--json` rows carry the same `gate` summary, and after a failed gate `af task run` names the
failing tests, at most 20, then `… and N more; af task show ID --checks`.

A Task's wall clock starts at its first `af task run`, not when it is planned: the preview says
`deadline starts when you run`, the Store moves the recorded deadline by the time the Task waited
for that run, so the confirmed plan is exactly the plan that runs, and the run prints the
deadline in local time with the time left (`deadline 17:41:18 +03 (2 h 59 min from now)`);
`af task show --json` carries it as `clock`. `af task plan` of a Task file whose content and bindings equal the planned Task's prints
the existing plan; a changed file under a planned ID is refused, and `af task plan --replace`
plans an unstarted Task again in place. A plan the Task's limits are too small for is refused
with the reason on stderr and the smallest limits that fit, computed from the reserve each
verification slot protects, a raised `wall_ms` keeping 5 s beyond the mandatory Attempts and the
reserve for the start of the run; `af task plan --fit` plans with them and prints what it raised,
and `--json` carries them as `planning_diagnostic.required_limits`.

Each Attempt runs under a token reservation that bounds it while it runs, and settles at the
usage its Provider reported. An Attempt whose Provider reported no usage — a Codex call that
failed with `Selected model is at capacity`, say, or one whose writer's lease expired while the
machine slept — is charged 0 and its usage is recorded as unknown, with its cause (`capacity`,
`rate_limit`, `authentication`, `model_unavailable`, `network`, `lease_expired`, `interrupted`
or `unreported`); it is never charged its reservation or an estimate
([ADR-0143](adr/0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)). It
adds nothing to the Task's, a node's or the verification reserve's charged tokens, so only the
Attempt limits bound repeated failures of this kind. `task list` prints such a Task's tokens as
`1200 (+1 unknown)` rather than as spend, `task show` adds the line
`tokens 1200 (+1 unknown: capacity)` naming the causes, and their `--json` documents carry
`unknown_usage_attempts`. A Claude Attempt whose bound model's safety classifier refused it
(`model_refused`) has known usage: it is charged what its result reports and is not retried,
because the same input is flagged again. A reply from a model the Worker did not bind is refused
and charged the same way, and a model ID Claude Code does not know fails as `model_unavailable`.
For each of these, `task run` and `review run` print one stderr line with the node, the Provider,
the bound model and effort, the classification and the next step; `task show` keeps the full
diagnostic ([ADR-0147](adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md),
[model bindings](task-execution/model-bindings.md)). The
Task ID is also the delivery confirmation. An unverified Task is evidence, not a deliverable:
read `task show`, fix the catalog, check, Worker or goal problem, then start a corrected Task
under a new Task ID.

## Retry a failed gate

A Task whose last round ended at a failed gate, its check not passing and no reviewer or
evaluator run after it, ends its `task run` and `task show` output with the command that runs
that gate again
([ADR-0196](adr/0196-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)):

```text
check kernel: stopped at tests step test (exit 100); rerun it with: python3 scripts/nextest-gate.py --locked --profile ci --test-threads 7
check kernel failed twice: af::it task_heavy::heavy_review_cannot_erase_stale_claims
check kernel host: load 61.20 at start, 48.03 at end, 14 CPUs, 7 test threads
next: af task run pagination-cli --retry-gate
```

When the failure was the host's rather than the candidate's (a loaded machine, a stale pin you
have since fixed outside the Snapshot), `af task run TASK_ID --retry-gate` runs the failed check
again on the same sealed candidate, in a new check Attempt of the same round, under the plan you
confirmed: nothing is re-planned or re-admitted, no Provider is probed and no new Task is made.
When the check passes, the Task continues into its review or evaluation as if it had passed the
first time; when it fails again, it ends at the gate again. `task show` adds `gate retried: NODE
ran again on the same candidate`, and the Task's accounting keeps the failed check Attempt beside
the new one. `af task show ID --json` carries the same facts under `gate`: `check_reports`, each
check node's latest `af/TaskCheckReport@1`, and `retries`, every `TaskGateRetried@1`; `af task
report` counts each `--retry-gate` as a run of the Task, and an `af task refresh` as none. The retry gives the check node one Attempt more than its plan did, so the Task's own
Attempt, token and wall limits, and its Pipeline's, must still hold it. It is refused, with the
reason and nothing recorded, for a Task that was verified, delivered, has not finished, or whose
last failure was not the gate's (its checks passed, or a reviewer or evaluator ran after them),
and when the plan's approval or deadline has passed or the limits leave no room.

## Look at a candidate

`af task findings ID` prints the Task's last review round: per reviewer, each finding with its
severity, confidence, title, location, the quoted evidence and the proposed fix, grouped by
severity, then the evaluator's verdict and its reason; `--round N` reads an earlier round, and
`--json` prints `af/task-findings@1`. `af task diff ID` prints the candidate's change against the
Task's source as the unified diff `git diff` prints for the same trees, without touching any
worktree; `--stat` and `--name-only` print its summary, `--snapshot` and `--base` pick two
Snapshots the Task recorded, and `--json` prints `af/task-diff@1`.

`af task materialize ID --into DIR` makes the tracked tree of the Git worktree `DIR` equal to the
candidate, with its writes, deletions and executable bits, and prints what it changed and
`next: git -C DIR status; af task report ID` (`af/task-materialized@1` under `--json`). It refuses
a `DIR` with uncommitted changes unless `--force`, and a candidate the Task did not verify unless
`--allow-unverified`, which then says `unverified candidate: review it before merging`. It never
commits or stages; `git -C DIR status` shows the change, ready for a person to commit.

```sh
af task findings pagination-cli
af task diff pagination-cli --stat
git worktree add ../pagination-review HEAD
af task materialize pagination-cli --into ../pagination-review
```

## Deliver the verified Snapshot

Choose an absent branch and an absent sibling path. The source checkout must still be clean at
the exact Task source commit.

```sh
task_id=pagination-cli
task_worktree="../$task_id"
task_receipt="${task_worktree}.delivery.json"
af task deliver "$task_id" \
  --repo . \
  --branch "af/$task_id" \
  --worktree "$task_worktree" \
  --confirm "$task_id" \
  --json > "$task_receipt"
```

Inspect `ignored_paths` in the JSON receipt before testing creates more files. These are
losslessly percent-encoded verified Snapshot paths that ordinary `git add` omits under the
repository and global ignore rules. They are identifiers, not literal pathspecs: decode their
filesystem bytes, then prefix Git's literal pathspec magic so names containing glob or
pathspec-magic characters identify only themselves.

```sh
python3 - "$task_receipt" "$task_worktree" <<'PY'
import json, os, subprocess, sys, urllib.parse

with open(sys.argv[1], encoding="utf-8") as receipt_file:
    encoded = json.load(receipt_file)["ignored_paths"]
paths = [urllib.parse.unquote_to_bytes(path) for path in encoded]
if paths:
    pathspecs = [b":(literal)" + path for path in paths]
    subprocess.run([b"git", b"-C", os.fsencode(sys.argv[2]), b"add", b"-f", b"--", *pathspecs], check=True)
PY
```

Alternatively, accept that the resulting commit differs from the verified Snapshot. Then inspect
and test the delivered worktree. It is deliberately uncommitted: committing, pushing and opening
a pull request remain human actions using the repository's normal Git controls.

A candidate verified in a second Task is delivered through that Task. A verified Task of kind
`verification` whose source it bound from another Task's candidate,
`"inputs": {"source": {"task": "pagination-cli", "port": "snapshot"}}`, and left unchanged,
delivers that candidate on the other Task's committed source: `af task deliver VERIFY_ID …` checks
the other Task as its own delivery would, and every delivery record names both Tasks through an
`af/TaskDeliveryProof@1`. Every other delivery still requires the Task's own Snapshot to descend
exactly from its source; a refusal names `af task materialize`, which writes a candidate into a
worktree of your choosing instead.

## Repeating and recovering a delivery

An exact repeat of the same command is safe. Before sealing, it verifies exact bytes or recovers
only an unchanged owned branch/worktree. After sealing, it verifies the durable delivery
identity and returns the same receipt even if the operator has since built, edited or committed;
that receipt attests to the original delivery, not the worktree's current bytes. A different
target for an already delivered Task conflicts.

If the process stopped after preparation, rerun the exact command. If recovery cannot prove that
partial content is delivery-owned, it records a terminal failure and preserves the original
branch and worktree. Retry the same explicitly confirmed Task with a new absent branch and
worktree; the next attempt releases only Afactory's internal ownership ref and never removes the
preserved target. Operator changes to an unsealed worktree are therefore never deleted.

## Warm checks

A code policy may declare a `[warm]` table
([ADR-0131](adr/0131-warm-task-checks-through-a-toolchain-keyed-bounded-cache.md)):

```toml
[warm]
build_cache = ["cargo_target", "cargo_home"]
caches = ["cargo"]      # optional: Cache Snapshots from machine policy
max_bytes = 8589934592  # optional: the eviction bound, 8 GiB by default; at most 32 GiB
hard_max_bytes = 17179869184  # optional: ends a running check; twice max_bytes by default
```

It grants a check directories that survive it. `cargo_target` becomes the check's
`CARGO_TARGET_DIR` and `cargo_home` its `CARGO_HOME`, Cargo's registry and git caches. Each is
`$XDG_CACHE_HOME/af/task-build-cache/<project>/<toolchain>/<kind>` (below `[storage]
build_cache_root` instead when the machine sets it), created with mode `0700` and
keyed by the repository and by the toolchain the check resolves. That toolchain is the
Snapshot's `rust-toolchain.toml`, `rustc -vV`, `cargo -vV`, the host triple, the check's
`PATH`, `LC_ALL`, `TZ` and `RUSTUP_HOME`, and the verified digest of a native toolchain the
check prepared ([ADR-0127](adr/0127-snapshot-pinned-rust-before-native-task-checks.md)). The
check's own runtime directory never reaches the key: wherever a hashed value names it, a fixed
token stands in its place, so two checks with the same toolchain share one key across Attempts,
Tasks and processes ([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)). A later
check with the same key starts from the earlier build. Both bounds cover the kinds of one key
together, not each on its own; the Storage Budget below bounds how many keys stay.

Under `[warm]` a check and its toolchain probe also receive the kernel's rustup home: its own
`RUSTUP_HOME`, else `$HOME/.rustup` when that directory exists. They also receive
`RUSTUP_AUTO_INSTALL=0`. The installed toolchain answers at once, and a toolchain the machine
lacks is `cold toolchain_unresolved`, never a download into the check's throwaway `HOME`. The
kernel only passes that path on. It never writes, bounds or removes the rustup home.

That toolchain is resolved once more before the Task's plan is admitted, so before any
implementer Attempt
([ADR-0196](adr/0196-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)):
when the source Snapshot's `rust-toolchain.toml` pins a toolchain the kernel's rustup home does
not hold, `af task start --execute` and `af task run` refuse the Task with the code policy file,
the `[warm] build_cache` field, the pinned toolchain and the probe's answer, and nothing runs or
is charged. Install it (`rustup toolchain install 1.88.0`) and run the Task again with `af task
run TASK_ID --confirm-plan PLAN_ID`. A `[rust_toolchain]` request with a machine mapping is
checked against the mapping the same way. A Snapshot that pins nothing names nothing to refuse.

The machine mapping is configured, not only exported: `[task] rust_toolchain_policy =
"~/.config/af/rust-toolchain.toml"` in the user's `~/.config/af/config.toml`, or in a project's
`.af/af.toml`, names the mapping file
([rust-toolchain.md](task-execution/rust-toolchain.md)), and
`AF_TASK_RUST_TOOLCHAIN_POLICY_FILE` still overrides every layer for one invocation. `af config
show --origin` names where it came from.

A declared `caches` kind gives the check an offline `CARGO_HOME` from the machine's cache
policy, as a Gate's `[gate] caches` do. That directory is materialized beside the check, never
into the source. It takes precedence over `cargo_home`: the two are never bound at once, and
the `cargo_home` kind records `cargo_home:superseded`.

`[warm]` refuses a great deal. It never runs with `require_container = true`: such a policy is
refused before any Worker starts. No directory ever enters a Worker sandbox, a Snapshot or a
delivered worktree, and its bytes never change a check's result. Before reuse the whole
directory is inspected without following links, relative to each parent's descriptor. It is
removed and recreated empty, so the check runs cold, when it is suspect. Suspect means a link, a
special file, another user's entry, a widened mode, a `credentials.toml` in a `cargo_home`, or
any entry the inspection cannot read. A check runs cold and says why when its toolchain cannot
be resolved, when another check holds its directory for 60 seconds, or when its directories are
already above `max_bytes`. The directories are measured again when the check ends and before
its result counts
([ADR-0135](adr/0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md)).
Above `max_bytes` only, they are evicted and the check's own result stands; the next check runs
cold. Only `hard_max_bytes` ends a running check: one that grew them past it fails with
`warm_cache_bound_exceeded`, however fast it was, and so does one that left anything the
measurement cannot read. The directories are then removed, never trimmed, and the observation
names the bound that acted. This is candidate-built state on your machine, not isolation.

`af task show` prints one line per warm check, named even when the check never started:
`check kernel: passed in 812345 ms, cargo_target warm 2147483648, cargo_home warm 409600`, or
`check kernel: not_run, never started, cargo_target cold deadline_exhausted`. Deleting
`$XDG_CACHE_HOME/af/task-build-cache` is always safe. A policy without `[warm]` records every
document exactly as before.

## What a check may write

Every check af runs — a Task code check, a measurement repetition, a review gate check and a
post-apply integration check — gets four directories of its own, created empty and private
before it starts and removed when it ends
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)):

| Variable | Directory |
| --- | --- |
| `HOME` | `<runtime>/home` |
| `TMPDIR` | `<runtime>/tmp` |
| `AF_CHECK_SCRATCH` | `<runtime>/scratch` |
| `XDG_CACHE_HOME` | `<runtime>/cache` |

`<runtime>` is `af-check-<pid>-<random>` in af's temporary directory; a killed `af` leaves it to
the next run's crash sweep. A container check sees the same variables below `/af-check`, on mounts
that go with the container. Write scratch files, copies of the tree and tool caches to
`$AF_CHECK_SCRATCH` or `$TMPDIR`, never to `/tmp` or `~/.cache`: nothing removes those, and a
check that copies its tree there on every run fills the disk. Build output that should survive a
check belongs in a `[warm]` directory or a Gate's `build_caches`. Review gate checks also get the
kernel's `RUSTUP_HOME` and `RUSTUP_AUTO_INSTALL=0`, as Task checks under `[warm]` do, so their
private `HOME` never downloads a toolchain; Cargo's registry comes from a Cache Snapshot or the
check's own `CARGO_HOME`.

## What a check reports

A Task code check also gets `AF_CHECK_REPORT`, a file in its runtime directory where it may
account for itself
([ADR-0196](adr/0196-rerun-a-failed-test-alone-report-the-gate-and-retry-a-failed-gate-on-the-same-candidate.md)):
one JSON object per line, each `"schema": "af.check-report/1"`. A `"record": "step"` line names
one step of the gate, its `stage` (`preflight`, `lint`, `build`, `tests` or `release`), its
`exit_code`, its `command` and, when the gate names one, the `fix` that repairs what it refuses;
one `"record": "tests"` line names the test threads, the tests that failed, and those that failed
in the parallel run and passed alone (`flaky`, with `"parallel": "failed", "isolated":
"passed"`). This repository's `scripts/ci-step.py` and `scripts/nextest-gate.py` write them.

After the check, af records an `af/TaskCheckReport@1`
([schema](../schemas/task-check-report-v1.json)) beside its `af/TaskCheckReceipt@1`: for each
local check that started, the 1-minute load average when it started and ended and the CPUs
available to af, which af measures itself, and what the check wrote, `admitted`, `absent` when it
wrote nothing, or `refused` with the reason when its lines are not this contract. A check's own
account never changes its result; its exit code does. `af task show` prints, per check, where it
stopped (`check kernel: stopped at preflight step performance-kernel-harness (exit 1); fix:
python3 -I scripts/performance/cli.py harness-digest --write`), its flaky tests and the tests
that failed twice, each on its own line, and the host's load beside a check that did not pass or
had flaky tests.

## The Storage Budget

af keeps at most `[storage] max_bytes` between runs
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)). The default is `"auto"`: 20 GiB,
or 40 GiB when the volume holding the warm build keys has more than 100 GiB free. One warm key of
a Rust workspace like this one is 11-16 GiB, and a 20 GiB budget evicted it after every run, so
every gate built cold; 40 GiB holds one 16 GiB key with half its size to spare and 16 GiB of
records beside it. `af storage` says which it chose and why. The budget counts warm toolchain
keys, warm Workspaces, review campaign Stores, the finished Tasks of every Task Store, Stores
this release cannot read, and installed versions. When af holds more, caches go first (warm
keys, warm Workspaces, installed versions), then telemetry, then records (review campaigns,
unreadable Stores, Surveys, finished Tasks), least recently used first within each. Nothing in
use goes — a held lock, a check holding a warm key, a live writer lease (judged when the sweep
takes stock, before it plans), a running `af review run`, the default or a pinned version, the
running binary — and nothing used within the last hour. Nor does the newest warm key of a
project whose checks used it in the last 7 days, a finished Task younger than `[storage]
keep_days` (14), a finished Task a registered delivered Workspace was made from (the Task behind
a delivered branch and its pull request), or a Project Store another release wrote, with or
without Workspaces (ADR-0158, ADR-0159). When the largest warm key does not fit `max_bytes` with
half its size to spare, `af storage` and the run say so once: `warm key 16.1 GiB does not fit
[storage] max_bytes 20.0 GiB: gates run cold whenever the budget must evict it; fix: raise
max_bytes to ≥ 25 GiB or lower [warm] max_bytes`. A finished Task goes through the Store's own
collection and keeps its tombstone. Every
directory is removed through descriptors opened from its configured root (`/` for a Store
registered elsewhere) without following a link, and only while it is still the directory the
sweep measured; anything else is left and reported. An installed version is checked again under
the lock `af self` holds while it changes the default or a pin, so one that became protected
stays.

`af storage` prints what af holds, per kind, against the budget, and the free bytes against
`[storage] min_free_bytes` (10 GiB by default); `--json` prints one `af/storage@4` document.
After every `af task run` and `af review run` the sweep holds af to `max_bytes`, evicting in the
order above; it does not collect by age unless `[storage] auto_gc = true`. A Task command prints
one line for all its sweeps, and only when they did something: `storage: freed 1.2 GiB (2 warm
keys)`, with what failed or what is still over the budget after it; `af task run --verbose` and
`af task start --execute --verbose` print every removal instead, as `af storage prune` does, and
only they repeat the Task's recorded `storage sweep:` line in a run's output. `af storage
prune` previews the sweep with collection; `af storage prune --apply` performs it now. It collects
first: finished Tasks beyond the
newest `keep_tasks` (20) of each Store, review campaigns beyond the newest `keep_campaigns` (20),
the Surveys of each Project beyond the newest `keep_surveys` (50) and Stores this release cannot
read, each idle at least `keep_days` (14); then it evicts until af fits. A Survey goes behind its
`SurveyCollected@1` tombstone, and never the newest complete one ([Collection](project.md#collection)). A Store made with `--state` is recorded in `$XDG_STATE_HOME/af/stores.toml`, so the
sweep reaches it too; it counts only af's own entries, and it is removed whole only while it
carries the owner marker `.af-store` af wrote when it created the directory and holds nothing
else. Otherwise only af's entries go, and the sweep names what it left in place. A finished
Task whose remote checks left gate branches or a pull request is
collected only after its gate cleanup is done, by the sweep and by `af task gc --apply` alike;
until then it stays and the next sweep tries again. A Task cancelled or abandoned after its gate
published is cleaned up the same way once no writer holds its lease. The sweep that ends `af task
run` records what it removed and what failed on that Task as one `storage_sweep` observation:
`af task show` prints a `storage sweep:` line and `--json` carries `storage_sweeps` (a telemetry
segment it removed is counted there among the omitted removals, never listed under another kind).
Telemetry is the kind `telemetry`: every registered telemetry directory, evicted segment by
segment under the same rules and past its own `[telemetry] max_bytes`, with every applied sweep
reclaiming its temporary files, unreferenced blobs and free index pages; `--json` of
`af storage prune` (`af/storage-prune@4`) carries that part as `telemetry`
([docs/telemetry.md](telemetry.md)).

Below the free-disk floor af does not start a check, a measurement repetition or a Worker
Attempt. Every volume af works on counts — the one holding its sandbox root (`[storage]
sandbox_root`, else the temporary directory, `TMPDIR`), the one holding `$XDG_CACHE_HOME` and the
one holding `[storage] build_cache_root` when it is set — and a volume whose free bytes cannot be
measured refuses too, with the measurement error. A refusal names the volume by its mount point
and file system type, both numbers and what af keeps there: `insufficient_disk: /tmp (tmpfs): 1.9
GiB (2040109465 bytes) free, floor 10.0 GiB (10737418240 bytes); the sandbox root is /tmp; set
TMPDIR or [storage] sandbox_root to a volume with room`. A configured `sandbox_root` is where
every sandbox and check runtime directory of a Task or review run is made, created private when
it is missing. The check's result is `not_run` with reason `insufficient_disk: …`; a
measurement fails with reason `insufficient_disk` without starting its command; a Worker Attempt
is released before it starts and charged nothing, and `af task run` stops with the same message,
leaving the Task to resume once there is room. `[storage]` is read only from the machine's layers: `/etc/af/config.toml`,
`~/.config/af/config.toml` and `AF_STORAGE__<KEY>`, for example
`AF_STORAGE__MAX_BYTES=50GiB`. A repository's `.af/af.toml` cannot change it; `af config show`
reports such a table as ignored. See `af help storage` for every key.

## Reclaim Store space

`af task list --sizes` prints, per Task, the bytes of the stored objects only that Task reaches
and the bytes it shares with other Tasks or Campaign records, then the Store's total. `af task
gc --older-than 14 --keep 5` previews which finished Tasks beyond the newest five, idle for 14
days, it would collect, how many bytes that frees, and why every other Task stays: running,
unfinished, holding a writer lease or bound by another Task's `inputs`. It writes nothing. With
`--apply` it writes one tombstone per collected Task and removes every object no remaining Task
or Campaign record reaches
([ADR-0135](adr/0135-collect-finished-tasks-behind-a-tombstone-and-a-reachability-sweep.md)). A
collected Task keeps its ID, kind, revision, outcome, spend, how many of its Attempts' usage was
unknown, and times: `task list` and `task show` print it as `collected <time>`, still with
`(+N unknown)` beside its tokens, and `task output` and `task deliver` refuse it. Run it
between Tasks: `--apply` is refused while any Task's writer lease is live. If it stops midway,
rerun it; the next run finishes the removal. The Storage Budget's sweep runs the same collection
by itself after every run.

## Report what Tasks cost

`af task report TASK_ID...` summarizes what one or more Tasks of one Store cost and how they ran,
as one Markdown block for a pull request description
([ADR-0142](adr/0142-carry-the-af-task-report-in-every-pull-request.md)). It takes the same
`--repo` and `--state` selectors as `task show`, reports the Tasks in the order given, and refuses
an unknown Task ID by name without printing anything else. It only reads the Store: no Worker
runs, no Provider is contacted, nothing is written.

```sh
af task report pagination-cli
af task report implement-x verify-x --json
```

The block leads with one line per pipeline the Tasks ran: `**name@version**:` and its steps in
dependency order, each named by its role with its Worker in parentheses (`codex
gpt-6-sol/high`, or `command`), the gate with its check names, and parallel steps of one role
grouped (`review (bugs, correctness: codex gpt-6-sol/high)`); Provider admission is left out.
A Task whose plan is no longer retained (it was collected) or that never reached planning names
no pipeline, and one line `**unknown pipeline**: not retained` stands for all such Tasks.
Then each Task is a round, numbered in the order given, with one row: its state as `task show`
states it, followed for a Task that did not verify by why (`finished/gate_failed/check_failed:
check kernel, 2 tests: a::b, c::d`), its review findings, chargeable tokens and active time. A Tokens cell, in a round, the
`Total:` row or a node, is the charged total followed by `(+N unknown)` when N of its Attempts
reported no usage, and the round's details name their causes
(`1 Attempt's usage unknown (capacity)`); a collected Task keeps the count, not the causes. **Findings** counts what the
Task's review rounds recorded, by severity (`6 major, 1 minor`), each finding once as the round's
reduce step wrote it; it reads `none` when the reviewers ran and their complete rounds found
nothing, `unknown` when the review ran but a round has no complete finding set (a required
reviewer's result is missing, say) or none was recorded yet, `gate failed` when a check failed and
the review did not run, and `—` for a Task without a review, followed by `; N reviewer(s)
failed` when a reviewer's Attempt failed. A round recorded before a `task
refresh` still counts. A last `Total:` row sums Attempts (with how many failed), tokens and
active time. **Active time** is the sum of the Task's runs, each from its first event to its
last: a run is `task start --execute` or one `task run` that started an Attempt, so a Task that
waited a day before it was resumed shows that day in its wall time only, and neither a `task
refresh`, even when it settles an Attempt an interrupted run left pending, nor a resume that only
publishes an already selected result is a run. **Wall time** runs from the Task's first recorded
event to its last. Each round then has a collapsed `<details>` element with its runs, wall time
and failed Attempts by reason class (`provider_failure`, `process_failure`, …) with the tokens
charged to them, and its nodes: role, Worker, Attempts, tokens, elapsed time and the checks the
gate ran with their durations. Each Attempt counts toward the Worker of the plan it ran under, so
a node that a refresh bound to another Worker has one row per Worker. Token counts read with
thousands separators (`205,295`). A figure the Store does not record reads `unknown`; nothing is
estimated. For a Task resumed once after an interrupt it prints:

```markdown
<!-- af-task-report:v1 -->
### af task report

**fixture/implementation@1.0.0**: implement (command) → gate (pagination) → evaluate (command)

| Round | Task | Outcome | Findings | Tokens | Active |
| ---: | --- | --- | --- | ---: | ---: |
| 1 | pagination-cli | finished/verified | — | 0 | 1.8s |
|  | Total: 4 Attempts (1 failed) |  |  | 0 | 1.8s |

<details>
<summary>Round 1 · pagination-cli: 2 runs, 3.5s wall, 1 failed Attempt (1 process_failure; 0 tokens)</summary>

| Node | Role | Worker | Attempts | Tokens | Elapsed | Checks |
| --- | --- | --- | ---: | ---: | ---: | --- |
| root.nodes.implement | implement | command | 2 (1 failed) | 0 | 258ms | - |
| root.nodes.check | check | - | 1 | 0 | 62ms | pagination passed 52ms |
| root.nodes.evaluate | evaluate | command | 1 | 0 | 96ms | - |

</details>
<!-- /af-task-report -->
```

Paste the whole block, both markers included, as plain text: the `PR report` check does not
count a block inside a code fence or indented as code, including one indented four spaces right
after a heading, a thematic break, a fence, an HTML block or a list item. Providers appear only as kind, model and effort,
and a recorded model that is not a model identity (a path or a URL, say) reads `unknown`: the
report never carries a Provider label, a path, a credential, a prompt or Worker output.
`--json` prints the same figures as one
[`af/task-report@2`](../schemas/task-report-v2.json) document, with exact decimal tokens and
times in milliseconds, the `pipelines` with their steps, and each Task's `round`, `state`,
`detail` and `findings`;
an unknown figure is absent there.

`af task report TASK_ID... --pr-body` prints the whole pull request description instead, which
`gh pr create --body-file` takes unchanged: the repository's `.github/PULL_REQUEST_TEMPLATE.md`
(or `--template FILE`) with this block between its markers, and the lines added block that the
checkout's `scripts/pr-lines.py` prints between `<!-- af-pr-lines:v1 -->` and
`<!-- /af-pr-lines -->` when the checkout has the script. `scripts/check-pr-report.py --body`
accepts it as it is.

## Troubleshooting

| Symptom | Meaning and action |
|---|---|
| `only a verified Task can be delivered` | Inspect the Task's findings (`af task findings ID`), checks and evaluator, then start a corrected Task; `af task materialize ID --into DIR --allow-unverified` writes the candidate where you can look at it. |
| `HEAD no longer equals the Task source revision` | Return the source checkout to the recorded commit or start a Task from the new commit. |
| `worktree is not clean` or `staged changes` | Preserve or commit the operator's work elsewhere, then retry from an exact clean source. |
| branch/path already exists | Choose a new absent target; Afactory never overwrites either one. |
| incomplete rollback/recovery | Rerun the exact command once. If it reports a preserved terminal failure, retain the original branch/worktree and retry the confirmed Task with a new absent target. |
| Worker authentication failure | Re-establish the named machine-local Provider login yourself with `af provider setup … --login` at a private terminal, then confirm with `af provider status`. |
| missing check tool | Install the repository-approved toolchain; never remove or weaken the check. |
| state or disk error | Preserve the external Task state directory, restore disk capacity/permissions, and retry the exact inspection or delivery command. |
