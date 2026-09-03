# Afactory

A multi-agent coding factory. `af review` runs a
sandboxed, budgeted reviewer pipeline against committed HEAD and folds the results into a
findings ledger with convergence. The boundary it enforces:
reviewers only ever mutate a private sandbox; they return typed findings, and only the
kernel integrates anything. Publishing to a branch or PR stays an explicit human action.

Minimal v2 provides `af task start --kind implement`: one implementer edits a private
sandbox, read-only acceptance gates inspect the sealed result, and an independent evaluator may
approve a content-addressed internal Snapshot. V3.1 adds an explicitly confirmed local delivery:
only a verified Task may create a new branch and linked worktree, and it still never commits,
pushes, opens a PR, invokes a remote, or changes the source checkout. The operating boundary for
trusted design partners is in [`docs/client-pilot.md`](docs/client-pilot.md).

The kernel's own vocabulary is defined in [`CONTEXT.md`](CONTEXT.md) — read it before
arguing about what a Finding, a Report, a Subject or a Scope is. Queued work lives in
[`docs/backlog.md`](docs/backlog.md), and decisions about the kernel's own design in
[`docs/adr/`](docs/adr/). **Picking up that work cold: start at
[`docs/workstream.md`](docs/workstream.md)**, which carries the status, the resume point, and
the gotchas that are not obvious from the code.

The legacy shell harness (`.agents/skills/self-review-heavy/scripts/`) is retired as the
orchestrator but kept deliberately: it is the reference implementation that regenerates
the synthetic fixture corpus, gated in CI.

## Install

One line installs the newest stable release into the self-managed layout
(`$XDG_DATA_HOME/af/versions/<v>/`, default symlink at `~/.local/bin/af`) while the repository is
private; `af` takes over from there:

```sh
gh api repos/PakhomovAlexander/afactory/contents/install.sh -H 'Accept: application/vnd.github.raw' | sh
af self setup-shell --write     # completions + man pages for your shell
af self status                  # what is installed, the default, and the pin that applies here
af self update --check          # exit 10 when a newer release exists; `af self update` installs it
```

A project's `.af/af.lock` pins the release that wrote it; inside such a project any `af` on
`PATH` execs that version, installing it on demand and verifying it against the release
checksums. `af help self`, `af help layers`, and `af help exit-codes` explain the rest; every
namespace and command has its own `--help`. Configuration merges built-in → `/etc/af` →
`~/.config/af` → every `.af/af.toml` above the repository → `.af/af.toml` → `.af/af.local.toml`
→ `AF_<TABLE>__<KEY>`; `af config show --origin` names where each value came from
([ADR-0044](docs/adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md)).

```sh
make check       # fmt + clippy + tests + fixture reproduction
make pilot-check # deterministic Task start/deliver/recovery/operator smoke
make fixtures    # prove the synthetic corpus still reproduces byte-for-byte
cargo run -p reviewctl --bin af -- onboard --help
cargo run -p reviewctl --bin af -- review tui
cargo run -p reviewctl --bin af -- provider status
cargo run -p reviewctl --bin af -- task start --kind implement --goal "describe the change" --authority HEAD --json
cargo run -p reviewctl --bin af -- task list --json
cargo run -p reviewctl --bin af -- task show TASK_ID --json
cargo run -p reviewctl --bin af -- task deliver TASK_ID --repo . --branch af/TASK_ID --worktree ../TASK_ID --confirm TASK_ID --json
```

## Onboard a repository for multi-agent review

`af onboard` is the binary-owned entry point for agents and operators. In a Git repository with
no `.af/`, plain invocation previews a deterministic `multi-review@1` authority and writes
nothing. `--apply` creates the absent directory atomically with a diff pipeline, exact lock,
correctness and architecture Worker packages, and `.af/README.md`: a standalone explanation and
pull-request operating workflow, so an agent does not need a pasted setup prompt.

```sh
af onboard                                      # inspect or preview
af onboard --gate 'check=make check' --apply   # apply when discovery cannot choose a Gate
af onboard --runner mixed --apply              # Claude correctness + Codex architecture
af onboard --refresh-lock                      # explicit repin after reviewed authority edits
```

The command is deterministic and token-free. It never executes a Gate or model, reads a
credential, fetches a pull request, creates Campaign state, commits, pushes, comments, or
overwrites existing authority. Once created, `.af/` is ordinary trusted project policy; plain
`af onboard` validates it, while lock refresh is explicit and changes only the selected pipeline
and its referenced Worker pins. See
[`ADR-0032`](docs/adr/0032-generate-review-authority-with-af-onboard.md) for the boundary.

Trusting configured Worker authority and intentionally starting a review Campaign or Task is the
authorization for Afactory to deliver each Worker its exact declared inputs, including retries
and later Rounds. Afactory does not ask for separate per-call confirmation; changing authority,
publishing, delivery, and other remote side effects remain separate operations. See
[`ADR-0033`](docs/adr/0033-configured-workers-authorize-declared-input-delivery.md).

Review Campaigns are light by default:

```sh
af review plan --policy-rev origin/main --base origin/main --uncommitted \
  --provider correctness=codex-work
af provider doctor --campaign pr-123 --policy-rev origin/main --base origin/main --uncommitted \
  --provider correctness=codex-work
af review run --campaign pr-123 --policy-rev origin/main --base origin/main --uncommitted \
  --provider correctness=codex-work --json
af review run --campaign release-audit --heavy --policy-rev origin/main --base origin/main \
  --uncommitted --provider correctness=codex-work --json
```

`plan` resolves policy, Base, candidate, exact Change Set, topology, Gates, budgets, and required
Provider bindings without Campaign state, external calls, or tokens. `provider doctor` runs the
same durable, charged admission used by `review run` but no Gate or Worker; its evidence is reused
by the exact Campaign. Every packaged Claude or Codex Worker requires a named provider from the
machine-local registry. An empty Diff is refused before all three external boundaries.

Light mode permits one closed review Round. If it finds defects, fix them and run the project's
deterministic gate; do not start another Campaign. `--heavy` preserves the pipeline's full
convergence window and is appropriate only when a human explicitly requests deep convergence
review. A Campaign must be resumed with the mode that opened it. See
[`ADR-0037`](docs/adr/0037-default-campaigns-to-one-round-light-review.md).

## Inspect Campaign history

`af review campaigns` lists the Campaigns under the default XDG review-state root without running
a Worker. Each entry includes its opaque ID and human label, pinned Subject/authority summary,
last closed Round and verdict, and the complete closed-Round history. Use `--format json` for the
versioned `af/review-campaigns@1` projection. `--state-root DIR` inspects an explicit root, including
legacy label-named state such as a repository's gitignored `.review/runs/` directory.
Unreadable or non-conforming entries are skipped and reported in the projection's `problems`
array, so one bad backup or stale directory never hides healthy Campaigns. Each entry also
carries the wall-clock its Rounds took and a Finding summary by disposition (open, pending, fixed,
rejected, wontfix, contested), read from the store's sidecar — never from the event stream.

```sh
af review campaigns
af review campaigns --format json
af review campaigns --state-root .review/runs --format text
af review gc --older-than 14 --keep 5          # preview: what would go, and how much
af review gc --older-than 14 --keep 5 --apply  # remove those Campaign directories
```

Each Campaign keeps its own CAS, so state grows with every Subject materialized — a large
repository costs hundreds of megabytes per Campaign. `campaigns` shows each Campaign's state
directory and last store write, and its size on disk with `--sizes` (a walk that costs seconds on
a large root); `af review gc` lists the Campaigns older than `--older-than` days
(never the `--keep` newest) and, only with `--apply`, removes their whole state directories.
Directories the enumeration cannot read are reported and left alone; nothing inside a kept
Campaign is ever touched.

New default state uses a deterministic opaque Campaign ID beneath the configured root; the label
is never interpolated into a new filesystem path. Existing label-named directories remain readable,
while ambiguous or escaping layouts fail closed. See
[`ADR-0035`](docs/adr/0035-address-campaign-state-by-opaque-id.md).

`af provider status` and the TUI's **PROVIDERS** tab inspect the machine-local Claude and Codex
authentication contexts without reading credentials. Codex ChatGPT logins also show the plan,
quota windows, utilization, and reset time exposed by Codex's local app-server protocol. Claude
Code has no headless usage command, so Afactory opens the fixed local `/usage` screen in a bounded
pseudo-terminal and reports the all-model and available model-specific weekly percentages (for
example, Fable). This compatibility probe does not
read OAuth credentials or start a billable model session. Both providers show used and remaining
percentages; Claude's localized reset text is deliberately not converted into a guessed epoch.

Pinned toolchain (`rust-toolchain.toml`), committed lockfile, `unsafe_code = "forbid"` — the
conventions for hub-owned Rust.

## Layout

```text
schemas/     the language-neutral contracts (JSON Schema 2020-12)
crates/
  review-core/   the Rust view of those contracts, plus the legacy importer
  review-parallel/ configurable bounded executor shared by
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
  review-runner/ reviewer adapters and the canonical gather barrier —
                 concurrency without nondeterminism
  review-sandbox/ materialized sandboxes, sealed mutation capture, and an
                 isolation level a pipeline can refuse
  review-graph/  the typed pipeline: named ports, deterministic planning, and
                 gating that suppresses dispatch structurally
  review-pipeline/ composition — the graph driving real capture, sandboxes,
                 checks, reviewers and the ledger
  review-config/ the pipeline definition format
  review-attempt/ attempt fencing and budgets that reserve before they spend
fixtures/
  legacy/        frozen real /self-review-heavy bundles — private per-hub data; the
                 template ships none, so the tests that read them are #[ignore]d
  synthetic/     16 cases generated by running the real harness
  adversarial/   four cases specified where no legacy behavior exists to capture
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
| `reviewer-result-v1.json` | what one reviewer attempt returned: reports, demands, disputes |

The schemas are the contract; the Rust types are one view of them. `tests/schema_parity.rs`
checks both directions — a populated value must validate, and a value the design forbids must be
rejected — because a schema that accepts everything passes a one-directional test.

Three things are deliberately unrepresentable rather than merely discouraged: a report has no
status field, a best-effort copy is not a `Capture` variant, and a patch proposal cannot name
zero claims.

`RunEvent`'s type vocabulary is closed by design but is not enumerated yet. It gets fixed with
the event store, when each event's payload is defined; enumerating it from prose now would make
the schema claim a completeness it does not have.

## Acceptance

`tests/legacy_corpus.rs` runs the contracts against real reviewer output. A contract that cannot
ingest real output unchanged is the wrong contract, and two corpora prove it from different
sides:

- **`fixtures/legacy/`** — frozen bundles of real reviewer output. Every stage output must parse
  under `deny_unknown_fields`, convert to `FindingReport@1`, validate against the schema, and
  satisfy the I-JSON numeric domain. This is private review data and stays in the hub that
  captured it. Those tests are `#[ignore]`d, so cargo reports them as `ignored` rather than
  as passing: a runtime skip would print `ok`, and cargo hides the explanation. A hub that
  has a corpus runs them with `make review-kernel-test-corpus`, where a missing corpus is a
  failure.
- **`fixtures/synthetic/*/input/`** — the stage outputs the real harness actually consumed,
  including the ones it was built to refuse. These ship with every checkout, so
  `the_contract_holds_on_real_harness_input` always runs: each output either converts whole,
  every finding keeping its `fix`, or is refused with a typed error — never a panic, never a
  half-converted batch. It asserts that both branches occurred, because a corpus that only
  converts proves nothing about strictness and one that only refuses proves nothing about the
  happy path.

The second exists because the first cannot ship. A skipped test reports `ok`, and `cargo test`
hides the notice that says why — so a checkout with no private corpus would have shown a green
acceptance suite that ran nothing.

The importer is stricter than the legacy schema in two places, each checked against the corpus
before being imposed:

- **`fix` is required.** The old schema allowed null. A claim with no proposed remedy is one a
  triager cannot act on — and every real finding in the proving corpus carried one, so nothing
  was lost.
- **The `(change-wide)` sentinel is dropped.** The harness wrote that literal string into the
  path field, sharing a namespace with real paths; an empty location list says the same thing
  without colliding.

It is also all-or-nothing per stage, where the harness skipped an unusable finding and kept its
siblings. That was right for a batch it could not re-request, but an importer that silently drops
claims would make the migration's ledger-equivalence test meaningless.

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

New Campaigns use the canonical identity policy: a selected Report artifact creates one
path-independent Finding unless an explicit relation or exact trusted occurrence key attaches it.
Each barrier publishes an immutable, Subject-bound `FindingSet@1`; legacy Campaigns retain their
recorded path/title fingerprint policy and permanent reader.

### Acceptance: the kernel reaches the harness's conclusions

`tests/replay_synthetic.rs` replays all 13 ledger cases by **parsing their own transcripts** and
driving the store with the same inputs, comparing three things against what the harness actually
printed: every `new= dup= reopened= escalated= open=` tally, every `converged` verdict with its
exit code and both counters, and the final ledger row by row. Cases are discovered from the
directory, so a new one is covered without touching the test.

`tests/legacy_ledgers.rs` imports every frozen ledger under `fixtures/legacy/` and every ledger
under `fixtures/synthetic/`, written by the same `ledger.sh` and carrying nothing private, so
the importer is exercised in every checkout.
It round-trips every row's decisions — and pins the documented loss in both directions: a note
rides on a resolution, and the importer emits one only for a row that is not `open`, so an open
row's note is deliberately absent. The frozen bundles are uniformly terminal and only ever
exercise the first branch; the synthetic ledgers exercise both. `tests/crash_replay.rs` kills the process at each boundary: the projection after
reopening equals the projection before, a crash between publishing an artifact and appending its
event leaves collectible garbage rather than a dangling reference, and a refused append does not
consume a sequence.

What the projection does *not* reproduce is the loss. Both reports of a same-round duplicate stay
attached with distinct artifact IDs, and a resolution never overwrites the note before it — both
asserted against the fixture that shows the shell ledger keeping only the later one.

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

`tests/hostile_git_config.rs` is [the adversarial spec](fixtures/adversarial/hostile-git-config.md)
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

Two properties the shell harness could not offer, each pinned against the fixtures it produced.

**Nothing overwrites.** `checks.sh` truncated `checks.tsv` at the start of every run, so the
round-3 gate in a frozen bundle whose gate ran five times left evidence of one.
Here each execution is an immutable `CheckResult@1` appended to the log, and a test asserts a
failing attempt's stderr is still readable after a later attempt passes.

**A check that could not run is not a pass.** `not_run` is a first-class status carrying a
reason, and it blocks a required gate exactly as a failure does. So does a gate with no required
checks: a vacuous run is the most dangerous green there is, because it looks exactly like a clean
one and asserts nothing.

### Typed argument slots

A check command is not a string. The harness ran `<name><TAB><shell command>` through `bash -c`
with `{tests}` filled from the bundle — which splices **paths taken from the diff under review**
into a command line. A file named `--config=/tmp/evil` is one `git add` away.

So the program and every option are trusted literals from project configuration, and values
derived from the change are `untrusted`: no leading `-`, no `@response-file`, no embedded NUL.
The identical bytes are fine as a literal and refused as untrusted — the check declares where
such values go, and nothing else can put them there. Refusal rather than quoting, because
quoting depends on the program's own option syntax, which the kernel does not know and must not
guess. A refused command is `not_run` with the reason recorded; it never reads as "ran and
failed", because nothing was verified.

Elapsed time is deliberately absent from the record: no policy reads it, and its presence would
make an otherwise reproducible artifact differ on every run — the legacy `checks.tsv` carried
seconds, which the fixture corpus has to normalize away in order to reproduce at all.

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

`tests/determinism.rs` proves both halves, which is the only way the first half means anything:

- Four reviewers, four delay patterns forcing different completion orders, and the test asserts
  the orders really did differ before comparing anything. Canonical admission produces identical
  event streams and identical ledgers every time, and the shared finding always belongs to the
  first node in canonical order with all four reports still attached.
- The control: the same outcomes admitted in completion order produce *different* ledgers, and
  the shared finding changes owner. Without that, the barrier would be proving nothing.

### Seeing exactly what a Worker receives

`af review render --node NODE`, with the same selectors as `plan`, prints the exact bytes that
Worker would receive on a first Attempt — the digest-pinned package instructions, the output
contract, and the Diff Subject Change Set for a model Worker; the typed `ReviewerInputs` document
for a command Worker — without creating Campaign state, calling a Provider, running a Gate, or
spending a token. The composition is the same function the adapters call at dispatch, so the two
cannot drift. Campaign-bound data is listed as omitted rather than invented: the Attempt
authority section, prior Findings, and the Gate decision exist only inside a Campaign. `--json`
returns `af/review-render@1` with the bytes, the context manifest, and the omissions; without it
the header goes to stderr and the raw input to stdout, so `> prompt.md` is exact.

### Command Workers

A package whose runner is not `claude` or `codex` is a deterministic command Worker. It runs
inside the materialized sandbox with that sandbox as its working directory, a cleared environment
(`PATH` and `LC_ALL=C` only), the typed `ReviewerInputs` JSON on stdin (no stdin when the document
is empty), and its stdout parsed as a `ReviewerResult`. The sandbox is a fresh materialization of
the Snapshot manifest and has no `.git`: nothing in it can read history, config, or hooks from the
reviewed repository. Such a Worker needs no Provider binding and spends no tokens, which is what
makes a small deterministic check the cheapest node in a pipeline.

## Sandboxes, and what this provider is not

`trusted_local` is a materialized copy of a snapshot in a temporary directory, optionally
read-only. **It is not security isolation.** A process running as the same user can `chmod` its
way out of read-only mode, read what the user can read, and open any socket.

It buys three real things: the review's *input* is immutable (a node runs against a copy, and
capture already happened, so the snapshot under review cannot be altered by anything the node
does), the environment is rebuilt from an allowlist rather than filtered, and every mutation is
captured. Note what that is not — an absolute-path write to the checkout on disk is not
prevented, and the case records that as open rather than calling it covered.

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
timeout/reap probe, ownership assertion, and v3 Gate route. This worktree could not run them
because its local daemon is unavailable; the route remains unverified until that job is green on
the candidate.

The distinction is enforced, not documented. Pipeline format v3 requires an explicit `[gate]`
Execution Binding. `provider = "container"` requires an OCI image pinned by digest and can
satisfy `required_isolation = "container"` only after a successful runtime probe;
`trusted_local` can satisfy only an explicit `none`
requirement. Gate checks then execute through that admitted provider in an independent
`ephemeral-write` COW clone. Their writes are discarded and reviewer clones still start from the
pristine template. Each resolution attempt is durable before its Gate receipt; same-Round retry
uses the latest observation while the append-only log retains failed admissions. `RunReport@4`
records that latest provider, pinned image when applicable, required and provided isolation,
mode, and admission result for every Gate node. Formats v1/v2 permanently retain their captured
local, read-only behavior. `GateDecision@1` also references a bounded mutation summary plus the
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
reconstructed from any artifact afterwards. Here an unwired input is a planning error.

**Gating is structural.** Once a gate blocks, every node downstream is *suppressed* — never
dispatched, and the test asserts the recorder saw exactly one dispatch. Suppression is transitive
and labelled with its root cause rather than the proximate one: a whole subgraph reading
"upstream missing" would bury the single fact that explains all of it. Suppressed nodes stay in
the report, because an absent node reads as "nothing to report", and `complete()` is false unless
every node actually ran.

A failed reviewer is a fact about the review, not a reason to lose the rest of it — its siblings
still run — but nothing may consume an output that does not exist, so its dependents are
suppressed. Plan order is a function of the pipeline alone (ties break by node ID), so two runs
on two machines produce the same report, including which nodes were suppressed and why.

The scheduler owns *when* and *whether*; the caller owns *what*. That split is why all nine of
these properties are proved with a recording stub — no models, no checks, no filesystem.

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

`tests/end_to_end.rs` runs it against a real git repository — real capture, real sandboxes, real
check and reviewer processes. The only stub is the reviewers' *judgement*, which is a `command`
runner emitting fixed findings: the one thing a test cannot supply honestly, and the one thing
the kernel deliberately knows nothing about.

Three properties, end to end:

- **A full review lands in the ledger.** Two reviewers report the same defect at different
  severities; the ledger holds one finding with both reports attached, at the higher severity,
  sourced in canonical order — and the checkout is byte-identical afterwards.
- **A failing gate means no reviewer ever runs.** Not "their output is discarded": the event log
  holds exactly one event, the `CheckCompleted@1` that failed, and the ledger is empty. A change
  that does not build produces no reviewer artifacts at all.
- **Two runs of the same review agree**, down to the fingerprint of every ledger row.

## Defining a pipeline

A review is described in a file rather than constructed in code —
[`.review/pipelines/heavy.toml`](../../.review/pipelines/heavy.toml) is the hub's own, and a test
asserts it loads, because a checked-in example that does not parse reads as a working reference.

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

Pipeline format v4 also makes every reviewer credential boundary explicit. Formats v1–v3 keep
their captured behavior and cannot acquire this claim retroactively:

```toml
version = 4

# The v3 [gate] binding remains required.

[[nodes]]
id = "correctness"
kind = "reviewer"
package = "correctness"
execution = { credential_mode = "brokered", operations = [
  { name = "model_inference", destination = "provider.openai", method = "responses.create", max_request_bytes = 1048576, max_response_bytes = 1048576, max_calls = 2, max_usage = 300000 },
] }
```

`credential_free` binds a reviewer that needs no credential. `brokered` requires a
machine-local connector and gives the adapter only an opaque `BrokerClient`; project authority
fixes each symbolic operation, destination, method, byte limit, call limit, and usage limit.
`trusted_unsafe` is the explicit compatibility class for a runner that can read reusable
credentials. It cannot authorize `auto_apply`. The current Codex and Claude CLI adapters report
`trusted_unsafe`; a v4 brokered pipeline without a broker-capable adapter and machine-local
provider is refused before any reviewer dispatch.

For each admitted Attempt, `ReviewerExecutionBound@1` records the exact mode, lease epoch, handle,
and operation policy; admission without that durable binding is invalid. The broker checks durable
authority before and after every connector call. Public revocation marks the handle immediately,
waits for an in-flight call, and prevents that call from releasing a response. A trusted connector
consumes raw authenticated wire bytes and returns only its decoded application response. Before
that response can cross the boundary, the broker rejects raw, base64/base64url, mixed-case hex,
and mixed-case percent forms of the credential. Connector errors and panics become normalized
charged receipts.

`BrokerOperationCompleted@1` is durable before any response returns and records only digests,
sizes, usage, and normalized outcomes. Fence races, credential exposure, numeric-domain overruns,
receipt failures, and terminal handle revocation all withhold the response. Attempt settlement
must cover durable broker usage; crash, timeout, and Round-supersession fences conservatively
charge the complete broker authority bound, or higher already-observed usage, so a connector that
finishes late can leave only a strictly validated revoked receipt. A late observed overrun raises
the durable charge above the earlier fence instead of losing its receipt. One refused request is
receipted before the handle becomes terminal, so repeated invalid or post-quota calls cannot grow
durable state without a policy bound.

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

`RunReport@5` records either one machine-path-free success receipt or an explicit failure for
every requested Gate/cache pair. Its receipt references a versioned `CacheManifest@1` containing
the sorted percent-encoded paths, content digests, and exact file sizes. Report publication
reverifies every referenced manifest and cross-checks it against the durable receipt, including
for incomplete reports. Machine policy is resolved only for an unresolved Gate, so replay of a
completed Gate does not depend on the original policy file or source still existing. A missing
mapping is an error, never an implicit read of `~/.cargo`; `max_files` counts directories and
files so directory-only trees are bounded. Durable cache failures are typed and path-free;
machine-local operator detail is emitted only to stderr.

**Format note.** The design's examples are YAML and this is TOML. The shape is unchanged and the
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
(attempt, node, fan-out, run), the tightest one refuses, and the error names itself, because a cap
that refuses anonymously is one nobody can raise correctly. A reservation is all-or-nothing across
scopes; an overrun commits rather than being refused, since the work is already paid for, and then
closes the gate on the next dispatch.

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
deterministic — while `af review report`, `campaigns`, and `ledger` do, so a person can see the
wall-clock of every Round and the usage of every Attempt. An absent row means not recorded, never
zero.
