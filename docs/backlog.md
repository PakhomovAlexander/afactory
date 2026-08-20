# Review Kernel — backlog

Work queued for `reviewctl` and the `/self-review-heavy` loop, in the order it should be done.
The vocabulary these items use is defined in [`../CONTEXT.md`](../CONTEXT.md). Decisions that
move a durable or security boundary are recorded as ADRs in [`adr/`](adr/), linked from the
milestone that made them.

Sequencing rationale: M0 freezes append-only contracts before new events are introduced. This
repository then reviews itself with the kernel it ships, so M1's triage visibility is on the
critical path. M2 establishes the trusted diff Subject, and M3 makes typed reviewer claims
authoritative before later evidence, patch, and scatter work.

---

## M0 · Freeze the append-only contracts

No feature may add another bare event literal or depend on Rust `Debug` output in persisted
state. These repairs precede M1 even though they add little user-facing behavior.

### M0.1 — Close the event vocabulary

`run-event-v1.json` gains a closed `event_type` enum and one Rust enum replaces every literal.
Today fourteen types exist and only four have named constants; M2 and M3 will add several more.
Adding an event means adding its enum variant, schema entry, payload contract, replay arm, and
fixture together.

### M0.2 — `RunReport@2`, typed everywhere

`publish_report` writes `format!("{verdict:?}")` and `format!("{reason:?}")` into the
append-only log, while `reviewctl run` reads the verdict with `.starts_with("Incomplete")` to
count closed Rounds. A Rust variant rename can therefore consume a Round with no compile error.

`RunReport@2` carries a verdict with a `kind` discriminant, missing nodes as data, and defined
suppression reasons. Round counting matches structurally. `RunReport@1` remains frozen and its
legacy reader permanent under
[ADR-0002](adr/0002-event-payload-changes-bump-the-type-version.md).

### M0.3 — Typed ports and exact invocation inputs

Current graph ports are names only. Extend node/reviewer contracts so every port declares an
artifact type/version, cardinality, optionality, and snapshot affinity. Planning rejects type,
cardinality, or affinity mismatches before dispatch, in addition to today's unwired-input check.

Before scheduling a node, persist one `NodeInvocation@1` with the complete input-port map and
exact artifact IDs. Completion seals one `NodeOutputReceipt@1` with the complete typed output map;
retries and replay reuse the recorded selection rather than querying ambient upstream output.
M2's Change Set and M3's Finding Set may not be wired until this foundation exists.

## M1 · Findings you can act on

Today a reviewer is *required* to produce a concrete `fix` for every finding, the importer
refuses a finding without one — and then no command can print it.

### M1.1 — Project `fix` from the canonical Report artifact

`crates/review-store/src/legacy.rs` already writes `fix` into the immutable CAS report and the
`FindingReported@1` event references that report. Do **not** add `fix` to the `@1` payload: that
would both duplicate the claim and violate ADR-0002's event-version rule. Make Ledger replay
resolve the referenced Report and project `body`, `fix`, and `confidence` from that one source.

`Finding.fix` is optional only for imported legacy ledger rows that have no Report artifact.
Their missing remedy must render explicitly as unavailable; replay must never invent one. Freeze
the existing duplicated `@1` payload fields for compatibility and treat them only as the fallback
for artifact-less legacy imports. Decision recorded in
[ADR-0005](adr/0005-report-artifacts-are-projection-authority.md).

### M1.2 — `reviewctl show --campaign NAME KEY`

Dereference `AttachedReport.report_id` into the CAS and print every attached report whole:
body, fix, confidence, per reviewer, per round — plus the full `history` and `current_note`.
This is strictly more than the projection can hold, because two reviewers who propose different
remedies both keep theirs.

Nothing new to store. `resolve` already opens the CAS; `show` needs the same.
An artifact-less legacy attachment renders the frozen projection fields and labels unavailable
fields explicitly; it is not treated as CAS corruption because ADR-0005 reserves that fallback
only for imported legacy rows.

### M1.3 — `reviewctl ledger --long`

The one-line form stays the default. `--long` adds body and fix under each row, so the common
triage case needs one command and one glance.

### M1.4 — `reviewctl report --campaign NAME [--format md]`

Restores a v1 capability: `ledger.sh report` emitted a markdown summary and the kernel has no
equivalent, so SKILL.md §5 currently asks the agent to hand-write a closing report from data it
cannot fully see. Emit rounds run, findings by severity and final status, resolutions with their
notes and evidence, spend per round, and the final verdict.

---

## M2 · The trusted diff Subject

The keystone. Today `Capture::committed` (`crates/review-source-git/src/capture.rs:164`) runs
`git ls-tree -r --full-tree HEAD` and keeps blobs only — no `.git`, no base, no diff — while
`ReviewerInputs` (`crates/review-runner/src/model.rs:261`) carries exactly one field,
`prior_findings`. Both reviewer prompts nonetheless open with *"Read the change in the working
directory you were given."* Reviewers are asked to review something the kernel cannot show them,
and cost scales with repository size instead of change size.

### M2.1 — `[subject]` in the pipeline definition

```toml
[subject]
kind = "diff"          # or "whole-tree"
```

Reviewer packages declare what they accept (`subjects = ["diff", "whole-tree"]`). A pipeline
whose subject no package accepts is a fatal load-time error — the same posture the graph already
takes for an unwired input port.

Each Round publishes `Subject@1`: kind and exact head Snapshot ID, plus Base and Change Set IDs
required for `diff` and forbidden for `whole-tree`. Review Selector labels may be retained as
metadata but do not participate in identity. Reviewer ports consume this artifact rather than an
ambient sandbox path.

The mutable refs are Review Selector inputs supplied by trusted invocation/administrator policy,
not read from candidate-controlled pipeline bytes. For example, a diff run receives
`--base origin/HEAD`; a whole-tree run receives an explicit authority ref even though it has no
diff Base. The candidate selector supports committed head capture and `--uncommitted`; dirty
capture uses the existing revalidated atomic boundary and fails closed on concurrent mutation.

### M2.2 — Trusted bootstrap, Base pinning, and campaign manifest

Before reading candidate content, trusted invocation policy resolves an Authority Snapshot. The
kernel loads the selected pipeline path, reviewer lock, reviewer packages, and project policy
from that Snapshot, resolves execution bindings, and records one immutable campaign manifest.
For a diff Subject the same Snapshot normally serves as Base; every later Round reuses its exact
Snapshot ID and never re-resolves the ref.

`CampaignOpened@1` records the Authority Snapshot, Subject kind, pinned Base when present,
resolved pipeline/config/package digests, execution-policy identifiers, convergence policy,
invocation focus, Finding identity policy, and deterministic genesis
`FindingSet@1`/`DemandSet@1` IDs. No node starts before those sets exist. A continuation uses that
stored manifest; a different manifest requires a new Campaign. The Review Selector ref is
resolution input, never identity. `parent_snapshot_id` is not available for the Base relation: it
means patch-integration lineage and `Capture::Derived` requires an `integration_batch_id`
alongside it. Decision recorded in
[ADR-0009](adr/0009-campaign-authority-is-base-pinned.md).

Before any Round node dispatches, `RoundStarted@1` records the exact Subject, prior Finding Set,
prior Demand Set, and campaign manifest IDs. Retrying an incomplete Round reuses those IDs. To
capture a changed head instead, an explicit restart appends `RoundInputSuperseded@1`, fences all
old attempts, and records replacement Subject/Set IDs under a new Round epoch; no selected output
from the superseded epoch may reach a reducer.

### M2.3 — `Repo::tree_diff`, allowlist untouched

See [ADR-0001](adr/0001-tree-diff-behind-a-typed-method.md). `SAFE_SUBCOMMANDS` keeps its five
entries; a typed method builds the whole argv from resolved tree ids and calls a private
unchecked runner. Obligations recorded in the ADR:

- the private runner keeps one call site and a comment saying why it exists;
- a test asserts the generic path still refuses `diff`;
- tree ids are constructible only from resolved revisions, never from a caller-supplied string;
- the typed method fixes diff algorithm, rename threshold, binary/full-index behavior, prefixes,
  color, quoting/`-z` parsing, locale, `--no-ext-diff`, and `--no-textconv`; it never inherits
  repository, user, or system diff configuration;
- `hostile_git_config.rs` gains a case: a candidate-controlled `.gitattributes` selecting a
  `textconv` driver must not execute, and the patch must equal a clean repository's;
- the git version goes into the Change Set artifact, because rename thresholds and binary
  handling are upstream behaviour the Change Set inherits.

### M2.4 — The Change Set as a wired port

Base/head Snapshot IDs, changed paths, rename map, canonical patch, Git version, and diff-policy
version are published as one content-addressed Change Set artifact and delivered through a port —
the pattern `prior_findings` already uses, rendered under its own heading as data rather than
woven into instructions.

**The sandbox root stays exactly the head tree.** Report locations and Proposal patches therefore
use the same repository-relative paths as the Subject and Change Set; no mode-specific prefix can
distort Report Scope or patch applicability.

### M2.5 — Report Scope

Each Report is stamped `in` / `out` — whether its location falls inside the Change Set of the
round it was made under. The kernel derives it; the reviewer never reports it. Convergence
evaluates active Report claims independently: a Finding blocks when any active claim is `in` at
the configured severity gate and is wholly `out` only when all active claims are `out`. Effective
blocking severity is the maximum severity among in-scope active claims, never an arrival-order
choice.

Convergence counts only in-set Findings. Out-of-set findings are recorded, keyed and triageable
in full — they simply do not block this Subject's verdict. A blocker found and thrown away must
never look like a blocker never found.

Known limit, accepted: a Finding that stops being re-reported keeps a stale scope. In practice
open findings are handed back each round with instructions to confirm or drop, so scope refreshes
for anything still live.

For a `whole-tree` Subject every Report is `in`. An invocation that crashes before Subject
capture emits no Reports and changes no prior Report Scope. Artifact-less legacy Reports that
predate Report Scope render `unknown` in the CLI as compatibility metadata; `unknown` is not a
third Report Scope value and cannot produce a passing scoped verdict.

Decision recorded in [ADR-0013](adr/0013-scope-is-evaluated-per-active-claim.md).

### M2.6 — Rename-aware Report Scope, identity untouched

The Change Set's path set includes both sides of every rename. A Report is `in` when it is
change-wide or any of its locations names either side; it is `out` only when every location lies
outside the set. Canonical Finding IDs introduced in M3 never contain a path, so a rename emits no
identity transition. Existing legacy campaigns keep their frozen fingerprint keys; a rename is
handled by recorded Grouping after M3 or by starting a new Campaign, never by rewriting old
events.

---

## M3 · Canonical claims and explicit dispositions

The live path validates `FindingReport@1` and then discards its identity semantics; it also
parses disputes but drops them and infers repaired claims from reviewer silence. M3 makes the
typed claim model authoritative on M2's immutable Subjects before fix verification depends on it.

### M3.1 — Complete typed Report ingestion and canonical Finding identity

The live path validates each legacy finding through `FindingReport@1`, then discards that typed
Report's relations and keys the Ledger by `sha256(file + "|" + normalized_title)`. That directly
contradicts the contract, which calls path/title a dedupe hint and permits auto-attachment only by
an explicit relation or an exact trusted occurrence key.

Make the reducer consume immutable `FindingReport@1` artifacts. A Report creates a deterministic
Finding ID from its selected Report ID unless it explicitly corroborates an existing Finding or
exactly matches a policy-trusted occurrence key. Disputes attach as relations but never collapse
claims. Existing fingerprint-based campaigns stay on a permanent legacy replay path; M2's
`CampaignOpened@1` records the canonical identity policy for new Campaigns. Decision recorded in
[ADR-0006](adr/0006-finding-identity-is-path-independent.md).

The runner's tolerant legacy wire parser remains an adapter only. Its selected output is bridged
into real artifact envelopes carrying Attempt producer, exact Subject Snapshot, and input IDs;
reviewer/round provenance does not leak back into the `FindingReport@1` payload.

Each ledger barrier emits an immutable, Subject-bound `FindingSet@1` from the prior Set plus
canonical selected Report/relation/resolution artifact IDs. Its deterministic reduction ID
includes reducer and policy versions; graph edges pass that exact Set ID to reviewers, gates, and
later reducers instead of querying ambient Ledger state.

### M3.2 — Prior-Finding dispositions become explicit

`docs/self-review-heavy.md` states that *"a dispute lands the finding as `contested`"*. That
behaviour does not exist; `Status::Contested` is reachable only by a human typing it.

Store each dispute as an immutable attached artifact — source, position, reason, Round, and
Subject — and move the Finding to `contested`. Blocking is unchanged because contested claims are
active. A Dispute grants no veto; it records the distinction from a Drop.

Do not continue treating omission as a Drop. Extend the reviewer result with an explicit
disposition for every prior Finding assigned to a required reviewer: re-report/corroborate,
`not_reproduced`, or dispute with a reason. Missing coverage makes the Round incomplete. Store
Drops immutably with reviewer, Round, Subject, and reason; trusted resolution policy decides what
they prove. Decision recorded in [ADR-0011](adr/0011-silence-is-not-a-drop.md).

### M3.3 — `reviewctl group <from> <into>` and `ungroup`

The escape hatch for reports that omit a corroboration relation or for genuinely ambiguous
duplicates. Grouping is operator-driven and event-recorded; it retains both IDs as aliases,
preserves all Report claims and histories, and makes the combined Finding active while any member
claim is active. `ungroup` appends a compensating event and reconstructs the independent views.
It never rewrites earlier events.

---

## M4 · Evidence and trusted resolution

Demands, Evidence, and fixed resolution are snapshot-sensitive, so they follow M2's Subject and
Change Set rather than being retrofitted onto whole-tree legacy events.

### M4.1 — Demands become durable obligations

`LegacyBenchmarkDemand` is `{claim, why, suggested_method}` and names no Finding. That is
deliberate: a demand can target a claim in a commit message or comment. Persist each selected
demand as an immutable artifact and project campaign-level state (`open`, `satisfied`, `waived`)
bound to the Subject snapshot. Pipeline policy, not reviewer prose, classifies a source's demands
as required or advisory. Required open Demands block convergence and carry into later Rounds.
Each demand barrier emits a deterministic `DemandSet@1` from the prior Set plus canonical selected
Demand, Evidence Satisfaction, and waiver artifact IDs; downstream nodes consume that exact view.

### M4.2 — `reviewctl evidence add` and explicit Demand waiver

`reviewctl evidence add <demand-id> <file>` stores the measurement content-addressed and links it
to the exact Demand and Subject snapshot. Trusted policy records satisfaction; the CLI cannot turn
an unrelated file into success merely by storing it. A head-Snapshot change makes prior
satisfaction stale unless policy explicitly admits reuse.

`reviewctl demand waive <demand-id> --reason ...` is the authenticated escape hatch. Resolving a
Finding, including as `wontfix`, never implicitly satisfies or waives a Demand. Finding resolution
may separately reference Evidence, but only an explicit Demand link changes Demand state.

The kernel can prove that Evidence exists, is linked, and was admitted by policy. It cannot prove
that the measurement methodology was sound; the benchmark-validity guidance remains the human
bar. Decision recorded in
[ADR-0007](adr/0007-demands-are-independent-blocking-obligations.md).

### M4.3 — External change attestation and Fix Verification

`reviewctl resolve ... fixed` currently lets an operator assert the terminal state directly.
Replace that path with `reviewctl attest-change <finding-id>`, naming the prior claim view, exact
current Subject/Change Set, changed regions, actor, reason, and Evidence IDs. The Attestation
moves covered claims to `pending-verification`; it grants no resolution by itself.

A trusted verifier consumes the current Finding view, explicit reviewer Drops/disputes/reports,
required checks, and snapshot-current Evidence. Only a positive `FixVerification@1` may produce
`Resolution(fixed)`. `reviewctl resolve` remains the authenticated ingress for `rejected` and
`wontfix-tracked`; neither state bypasses independent Demands. This same verification path is
reused by later kernel-integrated Proposals. Decision recorded in
[ADR-0012](adr/0012-fixed-requires-current-subject-verification.md).

### M4.4 — Non-fixed resolutions are scoped and challengeable

`rejected` and `wontfix-tracked` enter through authenticated, idempotent Resolution Requests
against an expected current Finding view. Both carry reason, actor, policy revision, Evidence IDs,
and Subject scope; `wontfix-tracked` additionally carries a maximum accepted severity, tracking
reference, and expiry.

An exact duplicate inside that scope remains terminal history. Materially new Evidence, a higher
severity, a Subject outside scope, or a persisted policy-time expiry emits an explicit challenge
and moves the Finding to `contested`. Replay never consults the host clock: policy time is an
artifact/event input. Decision recorded in
[ADR-0014](adr/0014-non-fixed-resolutions-are-challengeable.md).

### M4.5 — Convergence consumes exact final views

A passing Round requires every required node to complete, every Gate and Semantic Closure to
pass, no active in-scope Report claim at or above the severity gate, no required open/stale
Demand, and the configured clean-Round window over the exact final `FindingSet@1` and
`DemandSet@1`. Budget exhaustion, missing dispositions, stale Evidence, or missing outputs yields
incomplete/needs-human and does not consume a closed Round. Reaching the hard Round cap with
obligations open is exhausted, never pass.

Any head-Snapshot advancement or materially new/challenged claim resets the clean window. A
fixed claim remains News until a later complete Round verifies the same current head without
reopening it.

---

## M5 · Operating the tool

### M5.1 — `--format json`

Nearly free once M0.2 lands: print the structure that already exists. Text rendering becomes an
explicit formatter rather than `{:?}`, so both surfaces are deliberate and testable.

### M5.2 — Spend per round and per reviewer

Pure query work; the data is already in the log, tagged by node. `AttemptAdmitted@1` carries
`{selection, cost_tokens}`, `AttemptFenced@1` carries `{reason, charged}`, both stamped
`.node(node_id)`. `Kernel::attempts()` returns the whole `AttemptLedger` and its doc comment
reads "the operator's view" — `reviewctl` never calls it, so an operator cannot see that they
paid for a fenced attempt, which `review-attempt`'s own docs say is "something an operator needs
to see".

SKILL.md §5 already demands "spend per round" and the agent currently has no way to get it.
Feeds `reviewctl report` (M1.4).

### M5.3 — Campaign enumeration

No way to list campaigns or view a round-by-round history; state sits in `.review/runs/` with no
CLI over it.

Do not continue interpolating `--campaign` directly into a filesystem path. Store an opaque or
safely encoded Campaign ID plus a validated human label, reject separators/reserved traversal
forms, and prove the resolved state directory remains beneath the configured review-state root.
Enumeration prints ID, label, pinned Subject/authority summary, last closed Round, and verdict.

### Not building: `--only <node>`

A pipeline defines what a review *is*, so running a subset of it produces a different review
wearing the same campaign's name — and findings fold into the ledger during the graph run,
*before* convergence is computed, so a one-reviewer probe would bump `news_round` and leave the
other reviewer's priors unconfirmed while correctly failing to close a round. Churn without
progress.

`--pipeline FILE` already solves this properly: a `quick.toml` with one reviewer, its own
budgets, its own campaign, its own ledger. The gap is that nobody wrote it down — **document it
in SKILL.md as the cheap-iteration path**.

---

## M6 · Gate parity with CI

The original host-passthrough decision in ADR-0003 is superseded by
[ADR-0008](adr/0008-safe-caches-are-sandbox-local-snapshots.md).

### M6.1 — Gate Execution Bindings and `Mode::EphemeralWrite`

Route every Gate check through its resolved Execution Binding and admit the Sandbox Provider
against the pipeline's required isolation before execution. A safe pipeline requires the
container provider; `trusted_local` remains available only when policy explicitly accepts
`Isolation::None`, and that fact is visible in `RunReport@2`.

Within the admitted provider, the Gate uses `Mode::EphemeralWrite`. Each sandbox is an independent
COW clone of the template, so a writable Gate cannot leak mutations into reviewer copies. This
unlocks `tests/smoke-scaffold.sh` and `tests/smoke-update.sh`, whose checks need temporary writes.
Accepted loss: a passing Gate no longer proves its checks were read-only; sealing still proves
their mutations did not become Subject content.

### M6.2 — Sandbox-local cache snapshots

`Sandbox::environment()` sets `HOME` to the sandbox root (`review-sandbox/src/lib.rs:314`), so
every cache is cold. A pipeline may request a symbolic cache kind:

```toml
[gate]
mode = "ephemeral-write"
caches = ["cargo"]
```

Administrator policy maps that symbolic kind to exact credential-free subtrees, never to an
entire home cache root. The kernel preflights size and filesystem support, then reflinks or copies
those bytes into the sandbox under a hard byte/file limit. Cross-filesystem or over-limit copies
fail with a diagnostic rather than degrading into an unbounded multi-gigabyte copy. Writes stay
inside the sandbox and disappear at teardown.

Use offline package-manager mode so a cache miss fails loudly. Every Cache Snapshot kind, source
digest, size, and materialization method is recorded in `RunReport@2`. A direct host passthrough
is permitted only by an explicitly unsafe `trusted_local` execution policy and cannot satisfy a
pipeline requiring container isolation.

### M6.3 — Brokered external capabilities and revocation

A safe Attempt receives no reusable provider or service credential bytes through files,
environment, argv, stdin, logs, or model context. Privileged operations go through a trusted
broker using a non-readable handle bound to Campaign, node, Attempt, and lease epoch, with named
destination/method limits, response bounds, usage receipts, and budget policy. The broker checks
durable current authority on every operation; fencing or cancellation revokes the handle even if
the sandbox process survives.

Legacy runners that require readable credentials are classified `trusted_unsafe`. They cannot
satisfy a safe pipeline or an `auto_apply` binding. Complete the transformed-secret,
allowed-egress, and post-fence broker fixtures before M7/M9 claim those boundaries. Decision
recorded in [ADR-0015](adr/0015-safe-attempts-receive-handles-not-secrets.md).

---

## M7 · Patch proposals

ADR-0004 is refined and superseded by
[ADR-0010](adr/0010-proposals-are-exported-by-id-and-base-bound.md).
**Sequenced after M3** — a proposal scoped to a Change Set is worth far more than one scoped to
583 files.

### M7.1 — Reviewers author proposals; the kernel verifies

`PatchProposal@1` is fully typed in `review-core` with a schema and parity tests, and has zero
call sites in `review-pipeline` or `reviewctl`. Most of the machinery is already in place:
reviewers get `Mode::EphemeralWrite` sandboxes, sealing already derives what a node changed, and
the README already states the rule that derivation exists to enforce — "a proposal must equal the
kernel-computed diff, so an unreverted debug probe fails it rather than riding along".

An attempt may emit at most one atomic Proposal, referencing one or more Report/Finding IDs. Its
patch must equal the attempt's complete sealed diff after canonical normalization; a reviewer
cannot emit one independently selectable patch per finding from one shared sandbox mutation set.
A refused Proposal is absent, never stored carrying an unrelated edit.

SKILL.md's boundary turns out to be exactly right as written: reviewers never edit anything that
is *integrated*.

### M7.2 — `reviewctl export <proposal-id>`

`show <finding-id>` lists every linked Proposal ID and whether it is currently applicable. Export
names one Proposal exactly; `--finding` is only a convenience when exactly one current Proposal
exists and otherwise fails as ambiguous.

Verification is permanently bound to the Proposal Base in `base_snapshot_id`; it is never
silently re-verified on a later Round. When the Campaign head differs from that Proposal Base, the
Proposal is stale. Export refuses stale Proposals by default and requires `--allow-stale`, after
which the operator may deliberately use `git apply --3way`. This keeps `main.rs`'s "Nothing here
mutates any repository" boundary and makes stale application an explicit human decision.

### M7.3 — Revisit `[budgets]`

A model that writes code costs more than one that writes prose. Every pipeline's attempt and run
caps need re-derivation when M7.1 lands.

### Risk delta — recorded, accepted

**2026-08-20.** Reviewer-authored patches raise what a compromised reviewer can place before an
operator for application. M6.2 no longer grants a safe pipeline direct host-cache access;
ADR-0008 supersedes that part of the earlier risk acceptance.

`ContainerProvider` has never run against a live daemon, so the probes needing one stay open;
three of `malicious-check.md`'s six remain open; and `trusted_local` does not prevent an
absolute-path write to the checkout — recorded as open rather than claimed covered.

Accepted while this tool reviews only first-party code. Revisit before pointing it at code the
operator does not trust. Mirror this note into `fixtures/adversarial/malicious-check.md` so it
sits beside the probes it concerns.

### Inherent, not fixable here

Legacy corpus tests are `#[ignore]`d in the template because the fixtures are private per-hub
data. A hub that captures its own runs them with `make review-kernel-test-corpus`.

---

## M8 · Dynamic scatter, gather, and semantic closure

The recovered source design's Phase 4 was absent from the original M1–M6 reconstruction even
though the current budget enum's `Scope::FanOut` variant already exists. Rename that enum to
`BudgetScope` when wiring it; M8 restores the obligation without coupling it to M9's automatic
Integration.

### M8.1 — Persist a complete `SliceSet@1` before dispatch

A static partition or Planner reviewer emits bounded Review Slices for one exact Subject. Validate
stable unique slice IDs, path syntax, declared overlaps, coverage policy, maximum fan-out, and
collision-free tagged runtime node identity before admitting the complete SliceSet. No shard may
dispatch before `SliceSetAccepted@1` is durable.

### M8.2 — Scatter/gather is lossless and budgeted

Every shard receives the same Subject plus one Slice, owns a separate sandbox/attempt namespace,
and charges attempt, node, `BudgetScope::FanOut`, and Campaign budgets. Gather defaults to all
shards required and retains failed/missing shard status. Partial gather needs trusted explicit
policy and cannot silently satisfy a clean verdict.

### M8.3 — Dynamic slicing requires whole-Subject closeout

A required closeout reviewer examines the whole Subject plus gathered outputs after dynamic
scatter. Only administrator/Authority-Snapshot policy may waive closeout, and the waiver is
visible in the verdict. The adversarial cross-slice fixture must prove that two individually
clean slices cannot hide one boundary-spanning defect.

### M8.4 — Prove semantic-output closure

Every selected Report, Dispute, Drop, Demand, Evidence item, Proposal, check, and policy result
must reach its authoritative sink or a trusted explicit disposition. Static planning proves all
declared paths; Round finalization traces actual output receipts through gathers and reducers.
Missing or lossy routing yields incomplete/invalid, never pass.

---

## M9 · Internal derived Snapshots and automatic Integration

This completes the original accepted kernel boundary after M2's Subject model, M6's safe
sandbox/cache path, M7's verified Proposals, and M8's closure proof exist. Export remains the v1
human path; M9 is opt-in per trusted reviewer binding.

### M9.1 — Validate and compose selected Proposals

Only bindings granted `auto_apply` are eligible. Validate exact Proposal Base, claim/Evidence
links, protected paths, and pre-apply policy. Deduplicate byte-identical Proposals and compose only
non-overlapping changes in deterministic priority/node order; conflicts remain visible and never
receive an unreviewed semantic merge.

### M9.2 — Seal and check an unpromoted derived Snapshot

Apply the composed patch to an isolated tree, seal it as an immutable derived Snapshot, and run
configured post-apply checks against that exact ID. Failure leaves the prior head byte-identical;
the derived object remains audit evidence but cannot become the Campaign head.

### M9.3 — Promote at one transactional visibility boundary

Prepare all CAS artifacts first, then optimistically revalidate the expected Subject, Finding
view, Demand view, selected receipts, and policy revision. One commit records Proposal
application, advances the internal head, and exposes exact next views together. Crash recovery may
resume or abort a prepared operation but can never expose a partially advanced Campaign.

### M9.4 — Verification happens in the next Round

Integration moves covered claims to `pending-verification` and resets the clean window. The next
Round runs the full required graph on the derived head; only M4.3's Fix Verification path may
resolve the claims. Publishing the derived Snapshot to a branch or PR remains outside the kernel.

---

## Explicitly not doing

- **Changing fingerprint normalization in legacy campaigns.** ASCII-only lowercasing remains
  bug-compatible with the shell harness on their permanent replay path. New campaigns use the
  path-independent identity policy from M3.1; renames need no re-keying and ambiguous duplicates
  use recorded Grouping.
- **Adding `diff` to `SAFE_SUBCOMMANDS`.** See ADR-0001 — the allowlist is checked against
  `args[0]` alone and `bytes`/`text`/`line` take arbitrary argv, so admitting the subcommand
  admits every form of it workspace-wide, including the worktree-vs-index form that runs the
  candidate's clean filter.
- **Dropping out-of-set findings.** Information the operator paid frontier-model rates to
  produce; marked and non-blocking instead.
- **`--only <node>`.** A pipeline defines what a review is; `--pipeline quick.toml` with its own
  campaign already does this correctly and without touching the heavy campaign's convergence.
- **`reviewctl apply`.** `export <proposal-id>` plus `git apply` keeps `main.rs:15` true verbatim,
  and git handles a stale patch better than the kernel could. See ADR-0010.
