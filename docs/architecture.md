# Architecture

How the Review Kernel behind `af review` is built, and why each boundary sits where it does.
This is the long-form companion to the [README](../README.md): the crate layout, the contracts,
the store, source capture, Check nodes, reviewer adapters, sandboxes, the pipeline graph, pipeline
definitions, routing, and Attempt budgets. The vocabulary it uses is defined in
[`CONTEXT.md`](../CONTEXT.md); the decisions it rests on are recorded as ADRs under
[`adr/`](adr/README.md); the design notes it was ported from live under [`design/`](design/overview.md).

Every section names the test that pins the property it describes, because a documented boundary
that no test enforces is a wish.

## Layout

```text
schemas/     the language-neutral contracts (JSON Schema 2020-12)
crates/
  review-core/   the Rust view of those contracts, plus the flat reviewer-result reader
  review-parallel/ bounded executor shared by
                 filesystem-heavy infrastructure
  review-process/ bounded subprocess supervision shared by source capture,
                 reviewers, checks, and sandbox providers
  review-store/  canonical identity, the artifact CAS, the append-only log,
                 and the rebuildable Findings Ledger projection
  review-source-git/  offline, read-only capture of a tree as an immutable
                 content-identified snapshot, and materialization into a sandbox
  review-check/  typed Check nodes: trusted programs with typed argument slots,
                 per-attempt CheckResult records, and a gate that never passes
                 what it did not verify
  review-runner/ reviewer adapters: command and model runners behind one
                 contract
  review-sandbox/ materialized sandboxes, sealed mutation capture, and an
                 isolation level a pipeline can refuse
  review-graph/  the typed pipeline: named ports, deterministic planning, and
                 gates compiled into Task conditions
  review-pipeline/ composition — the graph driving real capture, sandboxes,
                 checks, reviewers and the ledger
  review-config/ the pipeline definition format
  review-attempt/ attempt fencing and budgets that reserve before they spend
  review-runner-claude/, review-runner-codex/  the model adapters: digest-pinned
                 Worker packages driving `claude -p` and `codex exec`
  review-source-task/ read-only capture of external issue sources into typed data
  af/            the `af` binary: onboarding, review, task, provider and self-management
fixtures/
  adversarial/   four attack cases, each specified before its test
  task-contracts/ the additive Task contract corpus
  task-runtime/  Task runtime scenarios (review, embedded review, bounded repair)
```

## Contracts

| Schema | Carries |
|---|---|
| `artifact-envelope-v1.json` | type, content and artifact IDs, producer, exact inputs, subject snapshot |
| `finding-report-v1.json` | one immutable claim by one attempt — no status, no round, no resolution |
| `finding-set-v1.json` | one Subject-bound canonical Finding view at a ledger barrier |
| `source-snapshot-v1.json` | content identity, never a branch; committed, synthetic-worktree or derived |
| `patch-proposal-v1.json` | an atomic change set naming the exact claims it covers |
| `run-event-v1.json` | the append-only stream envelope; `sequence` orders, never `occurred_at` |
| `check-result-v1.json` | one execution of one check, per attempt — `not_run` is its own status |
| `reviewer-result-v2.json` | what one reviewer attempt returned: reports, demands, and one disposition per assigned prior Finding |

The schemas are the contract; the Rust types are one view of them. `crates/review-core/tests/schema_parity.rs`
checks both directions — a populated value must validate, and a value the design forbids must be
rejected — because a schema that accepts everything passes a one-directional test.

Three things are deliberately unrepresentable rather than merely discouraged: a report has no
status field, a best-effort copy is not a `Capture` variant, and a patch proposal cannot name
zero claims.

`RunEvent`'s type vocabulary is closed by design but is not enumerated yet. It gets fixed with
the event store, when each event's payload is defined; enumerating it from prose now would make
the schema claim a completeness it does not have.

A `ReviewerResult@2` reviewer answers with flat `reports`; each one is converted to
`FindingReport@1` on its own and must satisfy it before any ingest admits it. `fix` is required,
because a claim with no proposed remedy is one a triager cannot act on. An empty `file`, or the
literal `(change-wide)` sentinel, is a change-wide claim and becomes an empty location list,
because the sentinel shares a namespace with real paths. One violation refuses every result at
that barrier, so a blocking Finding cannot degrade into an empty pass
(`crates/review-store/tests/canonical_identity.rs`).

## The store

Four layers, strictly one-directional — `canonical` -> `cas` -> `store` -> `ledger`:

- **`canonical`** — RFC 8785 canonical JSON and two domain-separated digests. `content_id`
  hashes the payload; `artifact_id` hashes the envelope. That split is what lets identical
  content be stored once while two provenance records stay distinct. Numbers outside the range
  it can format exactly are **refused**, not guessed at — a digest that is subtly wrong for
  large magnitudes is worse than one that fails loudly.
- **`cas`** — write temp, fsync, rename, fsync the directory. Raw payloads are addressed by
  `content_id`; typed envelopes are also stored and addressed directly by `artifact_id`, with both
  envelope and payload identity verified on read. A store that trusts filenames cannot detect
  corruption.
- **`store`** — SQLite in WAL mode, one writer, a dense per-run sequence that is the ordering
  authority (never `occurred_at`). It **refuses** an event referencing an artifact the CAS does
  not already hold, which turns a class of crash corruption into an immediate error.
- **`ledger`** — the projection. `rebuild` is its only constructor, so hand-edited state has no
  way in.

Every Campaign records the canonical identity policy, `report-derived@1`: a selected Report
artifact creates one path-independent Finding unless an explicit relation or exact trusted
occurrence key attaches it. Each barrier publishes an immutable, Subject-bound `FindingSet@1`.
The Ledger reads a Report only as an enveloped `FindingReport@1` whose locations are canonical
repository paths; any other Report artifact projects as an unreadable-authority placeholder that
blocks convergence.

### Replay and convergence

`crates/review-store/tests/crash_replay.rs` kills the process at each boundary of a canonical
Round: the projection after reopening equals the projection before, a crash between publishing
an artifact and appending its event leaves collectible garbage rather than a dangling reference,
and a refused append does not consume a sequence.

`crates/review-store/tests/ledger_convergence.rs` pins how the Ledger fold moves one Finding: a
fix that did not hold reopens, a higher-severity re-report escalates in place and is news, a
same-severity re-report is not, a rejected or `wontfix` claim is not reopened by a re-report,
`contested` blocks like `open`, a fix needs a clean Round before convergence, an open blocker at
the Round cap is `Exhausted` rather than a pass, and an open finding below the gate never
blocks. Nothing is lost on the way: both reports of a same-round duplicate stay attached with
distinct artifact IDs, and a reopen never overwrites the note the fix recorded.

Those scenarios set status with a bare-status `FindingResolved@1`, which a Review host writes
only as `contested`. Operator decisions are typed Resolutions, and they add one rule: an
in-scope re-report challenges a `rejected` or tracked-`wontfix` Resolution when its severity is
above what the Resolution accepted or its Subject is not the one the Resolution was decided on,
and the Finding becomes `contested` (`crates/review-store/tests/canonical_identity.rs`).

## The source adapter

A snapshot is identified by **content**: path, kind, executable bit, and the digest of the bytes.
Not by commit, branch, clone path, mtime or owner. The same tree in two unrelated repositories is
one snapshot, which is what makes "every reviewer inspected the same thing" a digest anyone can
recompute rather than an assumption.

Capture is offline and read-only, and the checkout is verified untouched afterwards. Two
providers, because the problem differs:

- **committed** — objects cannot change under the read, so one pass.
- **dirty** — there is no atomic read of a directory tree, so the boundary is *established*:
  fingerprint the index, take two complete passes, fingerprint the index again, admit only if all
  three agree, retry a bounded number of times, then fail closed. The failure it prevents is a
  torn tree — half the files from before an edit and half from after, digested as a state that
  never existed. A `CaptureObserver` seam lets the tests mutate the worktree *between* passes,
  which is the only honest way to show the boundary catches what it claims to.

### The hostile-configuration case, made executable

`crates/review-source-git/tests/hostile_git_config.rs` is
[the adversarial spec](../fixtures/adversarial/hostile-git-config.md)
turned into a test, and it found a real hole on its first run.

The premise is that the repository *is* the attacker: `.git/config`, `.gitattributes` and hooks
are all candidate-controlled, and capture runs before any sandbox exists, with the operator's
privileges. The test plants hooks, a clean/smudge filter, a textconv, `core.fsmonitor`,
`core.autocrlf`, an alias, and a submodule URL pointing at a closed port — then asserts that no
marker file appears and that the digest equals a clean repository's digest for the same content.

It failed. `git status` hashes worktree files, and hashing runs the candidate's `clean` filter —
so the read-only *check* was executing attacker-chosen code. There is no configuration that
disables an in-tree filter driver by name, because the name is attacker-chosen. The fix is a
`SAFE_SUBCOMMANDS` allowlist enforced in the git adapter: only `ls-tree`, `cat-file`, `ls-files`,
`rev-parse` and `rev-list` may run, `git status`/`diff`/`add` are refused outright, and
`worktree_state` was rebuilt from the index plus a direct filesystem walk. A control test runs
plain `git status` over the same repository first, proving the marker *would* have fired.

## Check nodes

Two properties. `crates/review-check/tests/check_runner.rs` pins them against real processes;
the vacuous-gate rule is pinned by `a_vacuous_gate_blocks` in `crates/review-check/src/gate.rs`.

**Nothing overwrites.** A gate that runs five times in one round leaves five records: each
execution is its own immutable `CheckResult@1`. `check_runner.rs` runs one check twice and
asserts that both executions' artifacts survive: the failing attempt's stderr is a distinct CAS
artifact, still readable after the later attempt passes.

**A check that could not run is not a pass.** `not_run` is a first-class status carrying a
reason, and it blocks a required gate exactly as a failure does. So does a gate with no required
checks: a vacuous run is the most dangerous green there is, because it looks exactly like a clean
one and asserts nothing.

### Typed argument slots

A check command is not a string. A shell command line with `{tests}` filled from the change would
splice **paths taken from the diff under review** into it, and a file named `--config=/tmp/evil`
is one `git add` away.

So the program and every option are trusted literals from project configuration, and values
derived from the change are `untrusted`: no leading `-`, no `@response-file`, no embedded NUL.
The identical bytes are fine as a literal and refused as untrusted — the check declares where
such values go, and nothing else can put them there. Refusal rather than quoting, because
quoting depends on the program's own option syntax, which the kernel does not know and must not
guess. A refused command is `not_run` with the reason recorded; it never reads as "ran and
failed", because nothing was verified.

Elapsed time is deliberately absent from the record: no policy reads it, and its presence would
make an otherwise reproducible artifact differ on every run.

## Reviewer adapters, and why gather has a barrier

The `command` adapter comes first deliberately: its output is a function of its input, so every
scheduling property can be proved before a model is ever invoked. A model adapter is then a
different runner behind the same contract, with nothing above it changing.

Every way an invocation can go wrong is a typed outcome — refused before execution, unavailable,
failed, malformed output — because a reviewer that crashed and a reviewer that found nothing must
never be indistinguishable. One of those means the change was reviewed. Unparseable output is
stored before it is parsed, so "malformed" stays a falsifiable claim.

**Concurrency is fine; nondeterminism is not.** Reviewers finish in whatever order the machine
felt like, and the projection is order-dependent — a finding belongs to its *first* reporter, and
later ones become duplicates. Ingesting in completion order would therefore let scheduling decide
who owns a finding, and a replay would not reproduce the run it replays. So results are admitted
in canonical order (by node ID), never in arrival order.

The pipeline's gather node is that barrier. It runs only once every reviewer has finished, and
`ReviewDomainState::run_gather` (`crates/review-pipeline/src/review_domain.rs`) flushes the
buffered reviewer events in node order; Ledger reduction then ingests the gathered results sorted
by reviewer node ID. The Task-host domain suite
(`crates/review-pipeline/tests/task_campaign_review/host/domain.rs`) checks the outcome: a Finding
that two reviewers report lists its sources in canonical order, not completion order.

### Seeing exactly what a Worker receives

`af review render --node NODE`, with the same selectors as `plan`, prints the exact bytes that
Worker would receive on a first Attempt — the digest-pinned package instructions, the output
contract, and the Diff Subject Change Set for a model Worker; the typed `ReviewerInputs` document
for a command Worker — without creating Campaign state, calling a Provider, running a Gate, or
spending a token. The Task host composes what it dispatches with the same functions, down to the
focus heading, so the two cannot drift. Campaign-bound data is listed as omitted rather than
invented: the Attempt authority section, prior Findings, and the Gate decision exist only inside a
Campaign. `--json` returns `af/review-render@1` with the bytes, the context manifest, and the
omissions; without it the header goes to stderr and the raw input to stdout, so `> prompt.md` is
exact.

### Command Workers

A package whose runner is not `claude` or `codex` is a deterministic command Worker. It runs
inside the materialized sandbox with that sandbox as its working directory, a cleared environment
(`PATH`, `LC_ALL=C`, and `HOME`, the XDG directories and `TMPDIR` inside a temporary runtime
directory of its own Attempt), the typed `ReviewerInputs` JSON on stdin, and its stdout parsed as a
`ReviewerResult`. The sandbox is a fresh materialization of the Snapshot manifest and has no
`.git`: nothing in it can read history, config, or hooks from the reviewed repository. Such a
Worker needs no Provider binding and spends no tokens, which is what makes a small deterministic
check the cheapest node in a pipeline.

## Sandboxes, and what this provider is not

`trusted_local` is a materialized copy of a snapshot in a temporary directory, optionally
read-only. **It is not security isolation.** A process running as the same user can `chmod` its
way out of read-only mode, read what the user can read, and open any socket.

It buys two real things: the review's *input* is immutable (a node runs against a copy, and
capture already happened, so the snapshot under review cannot be altered by anything the node
does), and every mutation is captured. Note what that is not — an absolute-path write to the
checkout on disk is not prevented, and the case records that as open rather than calling it
covered. The environment a check runs with is not the sandbox's doing: the check runner
(`review_check::CheckRunner`) clears it and rebuilds it from an allowlist, and a container run
starts from `--env-file /dev/null` plus the declared variables.

A `ContainerProvider` also exists for hosts with a usable runtime. Finding `docker` on `PATH`
proves nothing: detection runs the runtime's own `info` and requires it to succeed. An
installed-but-unusable runtime reports `Isolation::None` and refuses to exec rather than falling
back to the host. The invocation it builds is asserted exactly — one bind, `--network=none`, no
inherited environment. The image retains its own pinned `PATH`; only host-independent check
variables (`LC_ALL=C`, `TZ=UTC`, and declared additions) are reintroduced. The workload runs as
the caller's numeric UID:GID so writable-bind output remains usable and removable by the host.
Each execution also has a unique runtime name: if the supervised client fails or times out,
Afactory runs a bounded `rm -f` before sealing. An unconfirmed cleanup aborts the Gate and reports
the preserved sandbox path rather than sealing or deleting the possibly live bind.
`make review-kernel-container-probes` and its dedicated CI job carry the provider probes, live
timeout/reap probe, ownership assertion, and a v3 container Gate on the Task host; they stay
outside `make check` because a missing daemon must be a hard failure there, never a skip
disguised as success.

The distinction is enforced, not documented. Pipeline format v3 requires an explicit `[gate]`
Execution Binding. `provider = "container"` requires an OCI image pinned by digest and can
satisfy `required_isolation = "container"` only after a successful runtime probe;
`trusted_local` can satisfy only an explicit `none`
requirement. Gate checks then execute through that admitted provider in an independent
`ephemeral-write` COW clone. Their writes are discarded and reviewer clones still start from the
pristine template. Each resolution attempt is durable before its Gate receipt; same-Round retry
uses the latest observation while the append-only log retains failed admissions. `RunReport@6`
records that latest provider, pinned image when applicable, required and provided isolation,
mode, and admission result for every Gate node. Format v2 keeps its captured local, read-only
behavior. `GateDecision@1` also references a bounded mutation summary plus the
CAS digest of the complete v3 Gate mutation set, so permitted disposable writes remain observable
after the clone is discarded without becoming graph output.

### Sealing

Sealing consumes the sandbox handle and rescans the tree, diffing it against the manifest it was
materialized from. What a node changed is therefore **derived, never reported** — which is what
makes the auto-apply rule checkable at all: a proposal must equal the kernel-computed diff, so an
unreverted debug probe fails it rather than riding along. Kind is part of that identity, so
swapping a file for a symlink to the same bytes counts as a change.

## The pipeline graph

A pipeline is a DAG of nodes with **named typed ports**, not a list of stages. Everything that
can be known before running is checked at planning time — a cycle, a dangling dependency, an edge
to a port a node does not declare, or an input with nothing wired to it. The graph never starts
half-valid.

That last one is the interesting rejection. A reviewer declaring a `prior_findings` input and
wired to nothing would run happily and review an empty input with full confidence. The shell
harness had exactly this shape: prior claims reached a reviewer by being rendered into a prompt,
so what it actually received existed only inside a subagent's context and could not be
reconstructed from any artifact afterwards. Here an unwired input is a planning error. Every
reviewer and Scatter also declares one typed `FindingSet@1` input wired from Generation's exact
Finding Set: its `ReviewerResult@2` must disposition every prior Finding it was assigned, so a
reviewer without that wiring is refused when the pipeline loads, not mid-Round after its Gate.

**Gating is planned, then compiled.** A `gated_by` gate reaches through the graph: planning
resolves every gate a node depends on, directly or through an ancestor, so a pipeline declares it
once. The Review compiler turns each of those gates into a Task condition. A node behind a gate
that did not pass is *suppressed* — as an unselected branch, or as missing its upstream when its
predecessors were suppressed first — never dispatched, and never able to leave an artifact behind.
Suppressed nodes stay in the report, because an absent node reads as "nothing to report", and
`complete()` is false unless every node actually ran.

A failed reviewer is a fact about the review, not a reason to lose the rest of it — its siblings
still run — but nothing may consume an output that does not exist, so its dependents are
suppressed. Plan order is a function of the pipeline alone (ties break by node ID), so two runs
on two machines produce the same report, including which nodes were suppressed and why.

The scheduler owns *when* and *whether*; the caller owns *what*. That split is why these
properties are proved with a recording stub — no models, no checks, no filesystem.

## One review, end to end

`review-pipeline` is the composition layer, and the only thing it adds is wiring. That was the
test of the boundaries underneath: if composing them had required new rules, the split would have
been wrong.

```text
  capture ── snapshot ──> gate (admitted, ephemeral-write clone) ──decision──┐
                                                                              v
              architecture ┐   performance ┐   (each in its own ephemeral-write sandbox, gated)
                           └───────────────┴──> gather ──> ledger ──> convergence
```

The Task-host suites (`crates/review-pipeline/tests/task_campaign_review/host/`) run it through the
common Task runtime — real sandboxes, real check and reviewer processes. The only stub is the
reviewers' *judgement*, which is a `command` runner emitting fixed findings: the one thing a test
cannot supply honestly, and the one thing the kernel deliberately knows nothing about.

Four properties, end to end:

- **A full review lands in the ledger.** Two reviewers report the same occurrence at different
  severities; the ledger holds one finding with both reports attached, at the higher severity,
  sourced in canonical order.
- **A failing gate means no reviewer ever runs.** Not "their output is discarded": the failed
  `CheckCompleted@1` and the blocking Gate Decision are recorded once, no reviewer Attempt
  begins, and the ledger is empty. A change that does not build produces no reviewer artifacts.
- **A reopened host replays, it does not re-run.** A second host on the same Task selects the
  same recorded outputs, and the Gate's execution binding and cache receipts stay recorded once.
- **Two runs of the same review agree.** Two independent runs in separate stores publish the
  same Finding Set rows, Finding identity included.

## Defining a pipeline

A review is described in a file rather than constructed in code —
[`.af/pipelines/review.toml`](../.af/pipelines/review.toml) is this repository's own, and a test
asserts it loads through its lock and Worker registry, because a checked-in example that does
not parse reads as a working reference.

Everything is validated before a node runs, and every failure is fatal: a pipeline that is 90%
valid is not 90% of a review. Unknown fields are refused, so `gated_bye` is an error rather than
a setting that silently does nothing. A reviewer node with no runner is refused — it would be a
node that always reports nothing, which is indistinguishable from a reviewer that found nothing.
The graph's own validation reaches through unchanged, so an edge to a nonexistent port or an
input nothing feeds fails at load.

Argument provenance defaults to `literal`, because a project writing its own check command is
trusted; `untrusted` is the classification you have to type. The safe default is the one that
cannot be reached by forgetting.

New pipelines make Gate execution authority explicit:

```toml
version = 3

[gate]
provider = "trusted_local"       # use "container" for a safe pipeline
required_isolation = "none"      # safe pipelines require "container"
mode = "ephemeral-write"
caches = ["cargo"]                # optional symbolic request; never a host path

# A container binding instead uses:
# provider = "container"
# image = "registry.example/project-ci@sha256:<64 lowercase hex>"
# required_isolation = "container"
```

The binding is part of the captured pipeline artifact, so a later Round cannot silently change
its provider, image, or isolation policy. `af onboard` emits the explicit trusted-local form for
its documented first-party workflow; upgrading that policy to safe containment is a reviewed
edit with a project-toolchain image, never a generic moving tag.

A cache request is resolved twice: project authority names only `cargo`, while machine-local
operator policy selects the source and hard limits. The default policy path is
`$XDG_CONFIG_HOME/af/caches.toml` (falling back to
`$HOME/.config/af/caches.toml`); `AF_CACHE_POLICY_FILE` may select another absolute
file. Its v1 shape is:

```toml
version = 1

[cache.cargo]
source = "/absolute/curated-cargo-cache"
max_bytes = 4294967296
max_files = 250000
max_copy_bytes = 536870912
```

Pipeline format v4 also makes every reviewer credential boundary explicit. Formats v2 and v3 keep
their captured behavior and cannot acquire this claim retroactively:

```toml
version = 4

# The v3 [gate] binding remains required.

[[nodes]]
id = "correctness"
kind = "reviewer"
package = "correctness"
# Typed `inputs` and `outputs` as for any reviewer; only the credential boundary is shown.
execution = { credential_mode = "trusted_unsafe" }
```

`credential_free` binds a reviewer that needs no credential. `trusted_unsafe` is the explicit
class for a runner that can read reusable credentials, and it cannot authorize `auto_apply`. The
current Codex and Claude CLI adapters report `trusted_unsafe`. Before any reviewer dispatch, a
reviewer whose captured credential mode differs from the one its adapter reports is refused. Any
other `credential_mode`, or an `operations` list, fails at pipeline load.

The initial source is deliberately narrower than a complete Cargo home: it may contain package
archives under `registry/cache/` and sparse-index data under `registry/index/`. Unpacked
`registry/src/`, Cargo Git dependency caches, configuration, credential-shaped paths, symlinks,
special files, excess bytes or filesystem entries, and an over-limit cross-filesystem copy are
refused before check dispatch. Afactory walks through no-follow directory descriptors, retains
each admitted file descriptor across materialization, and bounds every read to its preflight size
plus one change-detection byte. macOS reflinks use the retained descriptor directly; before Gate
dispatch, Afactory removes source-controlled extended attributes, named forks, and ACLs from the
materialized file and applies fixed safe modes. It snapshots the admitted bytes under
`.af-cache/cargo`, sets `CARGO_HOME` plus `CARGO_NET_OFFLINE=true`, and removes the private cache
tree before sealing.

`RunReport@6` records either one machine-path-free success receipt or an explicit failure for
every requested Gate/cache pair. Its receipt references a versioned `CacheManifest@1` containing
the sorted percent-encoded paths, content digests, and exact file sizes. Report publication
reverifies every referenced manifest and cross-checks it against the durable receipt, including
for incomplete reports. Machine policy is resolved only for an unresolved Gate, so replay of a
completed Gate does not depend on the original policy file or source still existing. A missing
mapping is an error, never an implicit read of `~/.cargo`; `max_files` counts directories and
files so directory-only trees are bounded. Durable cache failures are typed and path-free;
machine-local operator detail is emitted only to stderr.

**Format note.** The examples in the ported design notes under [`design/`](design/overview.md) are YAML and
this is TOML. The shape is unchanged and the
loader is serde types, so another syntax is a different `from_str`, not a different model. The
reason is dependency risk: `serde_yaml` is archived and its forks are uneven, while `toml` is
the ecosystem default for Rust tooling configuration. Recorded rather than quietly done.

## Routing by changed paths

One pipeline for every change makes a docs-only commit pay for the full review graph. `.af/af.toml`
may declare routes, and the kernel selects the pipeline from the canonical changed paths — both
sides of every rename included — before anything is pinned or dispatched:

```toml
[routing]
unmatched = "default"   # or "refuse": no covering route is an error, never a silent fallback
ambiguous = "refuse"    # or "first": several covering routes pick the first declared
oversized = "scatter"   # the pipeline to select when a Worker's input alone exhausts its cap

[[routes]]
name = "docs"
paths = ["docs/**", "**/*.md"]
pipeline = "docs"
```

A route matches when every changed path matches one of its patterns. Patterns are anchored at
the repository root and match by segment: `*` within one segment, `?` one character, `**` any
number of segments; paths are matched in their canonical encoded form, so a non-UTF-8 byte is
`%FF` on both sides. Selection is deterministic and token-free; `af review plan` prints it
(`route    route => .af/pipelines/docs.toml (docs); 3 changed path(s)`, also `route` in
`af/review-plan@1`), Campaign open prints the same line, and the Campaign Manifest pins the chosen
pipeline, so later Rounds never re-route. An explicit `--pipeline` always wins and is reported as
`explicit`; a whole-tree Subject has no changed paths and follows the `unmatched` policy. Every
pipeline a route or the oversized policy names is authority: `af onboard --refresh-lock` pins them
all and `af onboard` refuses a missing or unpinned target.

The oversized policy is the bounded strategy for a Diff whose first-Attempt input exhausts a
Worker's cap: instead of refusing, plan and open switch to the named pipeline — typically a
Scatter pipeline whose shards each fit, with Complete coverage and a required closeout so no path
is dropped — measure it the same way, and report the switch (`replaced`). If that pipeline does
not fit either, the run still refuses before admission. Nothing is ever truncated.

## Attempts and budgets

An attempt is one execution of one node, and the process behind it does not necessarily stop when
the kernel stops waiting. A reviewer whose attempt was abandoned can still deliver a perfectly
plausible finding, and nothing about that finding looks wrong.

So an abandoned attempt is **fenced**, and anything arriving under a revoked epoch is
*quarantined*: recorded, charged, and unable to reach the FindingSet, convergence or the report.
Recorded rather than discarded, because "a fenced attempt delivered late" is something an
operator needs to see. Charged rather than forgiven, because a fenced attempt is not a free
retry — a test shows a cap bounding a retry loop precisely because the wasted attempts count.

Dispatching a retry fences its predecessor without an explicit timeout, so a superseded attempt
cannot win by finishing first.

Budgets **reserve before dispatch**, never account afterwards: a dispatch that cannot reserve does
not happen, so a retry storm or a wide scatter cannot overrun a cap by the width of one attempt —
and one attempt on a frontier model at maximum reasoning is not a rounding error. Scopes nest
(named token scopes, such as one node's or one fan-out's cap, inside the run), the tightest one
refuses, and the error names itself, because a cap that refuses anonymously is one nobody can raise
correctly. A reservation is all-or-nothing across scopes; an overrun commits rather than being
refused, since the work is already paid for, and then closes the gate on the next dispatch.

A Worker node may declare its own Attempt cap — `budget = { attempt = 150000 }` on the
`[[nodes]]` entry — and its dispatch then reserves that amount instead of `[budgets].attempt`,
at its own node scope as well as the run's. A cheap deterministic or comment Worker stops paying
for the deep correctness Worker's headroom. The cap refines `[budgets]` and requires it, cannot
exceed the run cap, and on a Scatter is each shard's reservation. `af review plan` prints every
static Worker's reservation and the largest amount they can hold at once (`reservations`,
`max_simultaneous_reservation` in `af/review-plan@1`); admission refuses before any Gate or
Worker when the run cap cannot admit that sum; `af review report` shows the cap that bounded
each Attempt. Pipelines without node caps behave exactly as before.

Before a Round admits any Provider, the kernel composes each model Worker's first-Attempt input
with the same function `af review render` uses and measures it against that Worker's cap. An input
that alone exhausts the cap — no room left for any output — is refused there, with nothing
dispatched or charged, and the refusal names the bytes, the tokens, the cap, and the bounded
alternatives: a narrower Base-to-candidate range, a larger `budget.attempt`, or a Scatter node so
each shard fits. Nothing is ever truncated silently. `af review plan` reports the same numbers per
Worker (`input_bytes`, `input_tokens`, `fits`, and `inputs_fit`) so the refusal is visible before a
Campaign exists.

How long an attempt took, and what its Provider reported per token kind, is recorded in a
**sidecar** table beside the event stream (`attempt_wall` in the same `events.sqlite`). Nothing in
identity, replay, the Ledger, or convergence reads it — the stream stays byte-for-byte
deterministic — while `af review report`, `campaigns`, and `ledger` do: all three show the
Campaign's wall-clock, and `af review report` also shows each Task Attempt's wall-clock and
Provider usage. An absent row means not recorded, never zero.
