# Review Kernel — shared language

The vocabulary of Afactory's Review Kernel and the `af review` command behind it. These words are
load-bearing: the kernel's guarantees are stated in them, and several of its invariants are
only expressible because two nearby concepts are kept apart. The mechanism itself is
documented in [`../../docs/self-review-heavy.md`](../../docs/self-review-heavy.md); this file
defines only the terms.

## Language

### Task execution

**Task**:
The durable business request, with immutable revisions, typed inputs and required outputs,
acceptance obligations, captured authority and bounded resources. Review is one Task kind.

**Execution Plan**:
The exact compiled Pipeline closure for one Task revision, including expanded child calls,
effective Workers, input identities, allowed effects, acceptance coverage and resource limits.
A Pipeline is the reusable public input/output definition from which a plan is compiled.

**Preparation Plan**:
An engine-installed fixed plan that runs the captured Planner Worker on the same Task ledger.
It protects future verification reserves and cannot satisfy business acceptance.

**Pipeline Proposal**:
A Planner's bounded generated Pipeline TOML. Compiler admission and durable selection establish
its provenance; the proposal itself grants no execution or developer authority.

**Developer Plan Decision**:
An authenticated approval or rejection of an exact Task revision, Execution Plan and captured
policy. Generated dependencies require approval before dispatch. A model response, writer label
or unsigned decision object cannot supply this authority.

**Shared Task Catalog**:
A Git-pinned set of Pipeline, Worker and Task-kind packages. Export uses typed public inputs as
parameters, shared defaults and separate contract fixtures; it carries no Task execution approval.

**Catalog Contract Fixture**:
An exact expectation for public interfaces, applicability, coverage and bounds. Passing it checks
package compatibility; independent Task acceptance still requires execution.

**Document**:
A rendered data artifact bound to an exact structured draft and captured sources. Document
acceptance requires current source/content checks and an independent evaluation; it carries no
code Snapshot identity. Captured source text supplies requirements and evidence, never execution
authority.

**Task Review Continuation**:
Exact independently verified repair evidence projected into the next admitted discovery Round.
It retains the original Round and claim identities, grants no complete-Review coverage and does
not create legacy Resolution authority. Later discovery can reopen a previously fixed claim.

**Captured Issue Source**:
An exact read-only observation of ticket fields, with raw source, per-field value and normalized
text identities. A source revision label alone is not immutable identity. It supplies Task
requirements and never execution authority; resume reads captured input without fetching again.

### What is reviewed

**Snapshot**:
An immutable, admissible source state carrying repository and capture provenance plus one Tree
Digest; its artifact identity is not interchangeable across repositories.
_Avoid_: "commit", "revision", "branch", "checkout" — every one of them names a pointer, and
a Snapshot's identity deliberately survives all of them.

**Tree Digest**:
The content-only identity of a Snapshot's ordered manifest of path, kind, executable bit, and
byte digest; identical trees have the same Tree Digest even when their Snapshots differ.
_Avoid_: using a Tree Digest where repository or capture provenance is required.

**Review Selector**:
The mutable, human-facing inputs used to locate what should be reviewed — branch name, PR
number, base ref, and authority ref — resolved into Snapshots and holding no authority over
identity.
_Avoid_: treating a selector as the thing reviewed; `origin/HEAD` is a Review Selector input,
never a Snapshot or Subject.

**Subject**:
The immutable object a reviewer is asked to judge, in one of exactly two kinds — `whole-tree`
(one head Snapshot entire) or `diff` (the delta between one Base Snapshot and one head Snapshot).
_Avoid_: "the change" used bare — it is only meaningful under a `diff` Subject, and saying it
under `whole-tree` describes something that does not exist.

**Base**:
The Snapshot a `diff` Subject is measured against, resolved once from the Review Selector's ref
at a Campaign's first Round and pinned by Snapshot ID for the Campaign's life.
_Avoid_: "parent" — `parent_snapshot_id` already means patch-integration lineage (S1 produced
by integrating a validated patch into S0) and cannot carry this relation.

**Proposal Base**:
The exact head Snapshot from which a reviewer's sandbox and Proposal patch were derived, encoded
by the legacy field name `base_snapshot_id` in `PatchProposal@1`.
_Avoid_: **Base** — for a diff Subject, Base is the comparison Snapshot and Proposal Base is the
current head.

**Change Set**:
The derived, content-addressed description of a `diff` Subject: the changed path set and the
patch between Base and head. Recomputed every Round, because the Base is pinned but the head
advances.
_Avoid_: "the diff" when identity matters — a Change Set is an artifact with a digest, not a
command's transient output.

**Authority Snapshot**:
The trusted Snapshot from which a Campaign resolves its pipeline, reviewer lock, and project
policy before candidate content can influence execution authority.
_Avoid_: assuming Authority Snapshot and Base are synonyms; a diff Campaign commonly uses one
Snapshot for both roles, while a whole-tree Campaign still needs authority without having a Base.

**Campaign Manifest**:
The immutable resolved authority of one Campaign: exact Authority Snapshot, pipeline, lock,
reviewer package, policy, convergence, budget, focus, and genesis-root artifact IDs.
_Avoid_: calling it the Authority Snapshot; one trusted source Snapshot can resolve different
Campaign Manifests under different trusted invocation policies.

**Report Scope**:
Whether a Report's location falls inside the Change Set it was made under — `in` or `out`.
Attached deterministically to each Report claim from its exact Round Subject, never supplied by
the reviewer and never stamped on the Finding, because a file this branch has not touched yet may
be touched by a later Round. A Report with no derivable exact Round Subject is presented as
`unknown`; that is fail-closed compatibility metadata, not a third Report Scope.
An unavailable Subject or Report authority is counted in the active clean window and is persisted
as `authority_unavailable` in new `RunReport@3` conclusions only when no real Finding or failed
Gate is already the cause; terminal output alone is never the only explanation for a failed
convergence decision.
_Avoid_: unqualified "scope" or "out of scope" as a dismissal; an out-of-set Finding is real,
recorded, and triageable — it simply does not block this Subject's convergence.

**Review Slice**:
A persisted, bounded subset of one Subject assigned to a reviewer shard by static policy or a
Planner.
_Avoid_: treating a Slice as a smaller Subject; shard results still belong to the whole Subject
and dynamic slicing requires whole-Subject closeout.

### How a review runs

**Campaign**:
One logical review, spanning as many Rounds as it takes to converge, owning one event log and
one Ledger.
_Avoid_: starting a second Campaign to escape an inconvenient Ledger; re-invoking the same
name continues the same review.

**Campaign Mode**:
The trusted invocation choice that bounds how many closed Rounds a Campaign may dispatch.
`light` is the default and closes after exactly one completed review Round; its next step after
Findings is deterministic repair and project Gates, not another Campaign. `heavy` is explicit
human-directed opt-in and retains the pipeline's full convergence window. The effective policy is
pinned in the Campaign Manifest and cannot change on resume.
_Avoid_: interpreting a finding-bearing light result as a request to open a fresh Campaign.

**Round**:
One immutable Subject/input-Set execution of the pipeline within a Campaign. Only a Round that
reaches a real verdict *closes*; an incomplete one resumes those exact inputs unless explicitly
superseded, and never consumes a clean/cap slot merely for failing to complete.
_Avoid_: "run" as a synonym - an `af review run` invocation may fail to close a Round at all.

**Gate**:
The Check nodes that must pass before any reviewer is dispatched. A Check that could not run
is not a pass, and neither is a Gate with no required Checks.
_Avoid_: "the build" — a Gate is whatever the pipeline declares, and a vacuous one is its most
dangerous state.

### What comes back

**Report**:
One immutable claim by one reviewer attempt — severity, location, body, and a concrete fix.
It carries no status, no round, and no resolution.
_Avoid_: "finding" for this; a Report is evidence, and several may attach to one Finding.

**Finding**:
The Ledger's unit of triage: the durable identity a defect holds across reviewers and Rounds,
carrying status, severity, and every Report attached to it.
_Avoid_: "issue", "bug" — both imply a tracker outside this system.

**Grouping**:
A recorded, reversible adjudication that two Findings represent one defect, preserving both
Finding IDs, every attached Report, and every Report's independent verification obligation.
_Avoid_: "merge" — grouping does not erase a claim, rewrite history, or make one verification
cover another.

**Dispute**:
One reviewer's recorded position that an existing Finding is wrong, naming the Finding and
carrying a reason. Attached immutably like a Report; it moves the Finding to `contested`,
which blocks convergence exactly as `open` does.
_Avoid_: reading a Dispute as a veto — it never lowers what blocks, it raises what a human
must adjudicate. And never conflate it with a Drop: a Dispute says the claim was wrong, while a
Drop says it is no longer reproduced on this Subject.

**Drop**:
One reviewer's explicit, immutable position that a prior Finding assigned to it is no longer
reproduced on the current Subject.
_Avoid_: inferring a Drop from silence; an omitted required disposition makes the Round
incomplete.

**Demand**:
A durable, snapshot-scoped obligation to measure a performance claim, naming the claim in prose
rather than a Finding because it may live in a commit message or comment instead of the Ledger.
_Avoid_: treating a Demand as attached to a Finding or as disposable round output; it is an
independent Campaign obligation.

**Evidence**:
The content-addressed measurement artifact linked to the exact Demand or resolution it supports
and to the Subject snapshot it measured.
_Avoid_: "the benchmark" as a number in a note — the kernel can check that Evidence exists and
is linked, never that the measurement behind it was sound.

**Change Attestation**:
An authenticated claim that an external change on an exact Subject is intended to address named
Finding/Report claims, carrying lineage and Evidence but no resolution authority.
_Avoid_: "mark fixed" — an Attestation moves work to verification; it does not prove success.

**Fix Verification**:
A trusted policy result that named active Report claims are no longer reproduced on one exact
Subject and that every required check and Evidence obligation passed there.
_Avoid_: treating patch application, a Drop, or a Change Attestation alone as verification.

**Resolution**:
A trusted, evidence-bearing disposition of named Finding/Report claims, scoped to a Subject and
policy revision and challengeable by materially new evidence.
_Avoid_: an unqualified status toggle; `wontfix-tracked` also carries a severity ceiling and
expiry.

**Ledger**:
The projection of a Campaign's Findings, rebuilt from the event log and the immutable artifacts
its events reference.
_Avoid_: treating it as storage — it is never written directly, and hand-edited state has no
way in.

**Finding Set**:
An immutable, Subject-bound Ledger view at one deterministic reduction boundary, passed to nodes
by artifact ID.
_Avoid_: an ambient query for "current findings"; every consumer names one exact Finding Set.

**Demand Set**:
An immutable, Subject-bound view of open, satisfied, stale, and waived Demands at one deterministic
reduction boundary.
_Avoid_: a list of whatever Demand events happen to exist when a node starts.

**Proposal**:
A reviewer-authored atomic patch linked to one or more Report/Finding claims, admitted only when
it equals the diff the kernel computed by sealing that reviewer's sandbox.
_Avoid_: "the fix" — a `fix` is the prose remedy every Report must carry; a Proposal is the
executable one, and a Finding can have the first without the second. Export is human-directed;
automatic Integration, when policy permits it, advances only an internal derived Snapshot.

**Integration**:
A trusted, transactional promotion of one or more validated Proposals into a new internal head
Snapshot after combined-tree checks pass.
_Avoid_: "apply to the repository" — Integration never writes a source checkout, branch, or PR.

**Attempt**:
One execution of one node. An abandoned Attempt is *fenced*, and anything arriving under a
revoked epoch is quarantined — recorded, charged, and unable to reach the FindingSet.
_Avoid_: treating a fenced Attempt as a free retry; it spent tokens and the budget knows.

**Budget Scope**:
The accounting boundary charged by one Attempt — attempt, node, reviewer fan-out, and Campaign.
_Avoid_: **Report Scope**; Budget Scope governs spend, never whether a claim blocks.

**Execution Binding**:
The trusted, content-pinned sandbox, tool, network, environment, broker, and quota policy under
which one executable node runs.
_Avoid_: an ambient/default environment; an unresolved or insufficient Binding fails planning.

**Provider**:
An operator-named, machine-local authentication context for one adapter kind. A Provider ID is a
stable label for that context, not a verified or immutable external account identity: credentials
inside the context can change without changing the ID. An ambient CLI login may be displayed as an
explicitly unstable candidate but does not become a configured Provider until the operator names
its context. Credentials are capabilities used through a Provider and never part of its ID.
_Avoid_: using "provider" for a CLI binary, model, reviewer package, or verified account identity.

**Provider Operation**:
A durable, Round-bound machine-local operation that proves one configured Provider can satisfy one
adapter capability before dispatch. Its epoch fences stale continuation, failed and abandoned
work is charged, and persisted state contains only non-secret handles and normalized failure
metadata.
_Avoid_: **Attempt**; an Attempt consumes an admitted Provider rather than authenticating it.

**Provider Admission**:
The current successful result of a Provider Operation's structural authentication probe and
bounded real-inference smoke test for one adapter capability. Admission is local execution state,
not pinned pipeline authority, and expires when its Round, Provider label, or capability changes.
_Avoid_: treating an ambient login or a Provider ID as proof of usable authentication.

**Broker Handle**:
A non-secret, Attempt/epoch-bound capability authorizing only named external operations through a
trusted broker that can revoke it after fencing.
_Avoid_: passing reusable provider credentials into a reviewer sandbox.

**Cache Snapshot**:
A bounded, credential-free copy or copy-on-write clone of an administrator-approved dependency
cache, materialized inside one sandbox and discarded with it.
_Avoid_: "passthrough" — a direct host mapping is not a Cache Snapshot and cannot satisfy safe
isolation.

**News**:
Whether an in-scope claim was created, reopened, materially escalated, challenged, or moved to
pending verification inside the clean window, independent of its current status; an exact
corroborating re-report is not News.
_Avoid_: reading a fixed Finding that still blocks convergence as a bug; that is News doing its
job.

**Convergence**:
The kernel's verdict over one exact final Finding Set, Demand Set, required-node/gate state,
Semantic Closure, budgets, and clean-Round window.
_Avoid_: a reviewer verdict or an empty Finding list; neither proves the required review ran or
that all obligations closed.

**Semantic Closure**:
The proof that every verdict-bearing output from every selected node reached its authoritative
reducer, gate, verifier, or integrator, or an explicitly trusted disposition.
_Avoid_: assuming a successfully completed graph is closed; an unwired Finding or Demand makes
the result incomplete.

## Relationships

- A **Campaign** has many **Rounds**; a Round produces many **Reports**; Reports fold into
  **Findings**, many-to-many across reviewers and Rounds.
- A new **Report** creates a new **Finding** unless it explicitly corroborates an existing one or
  exactly matches a trusted semantic occurrence key; path and title are grouping hints only.
- **Grouping** changes adjudication, never identity: renames and rewordings do not change a
  Finding ID.
- A required **Demand** remains blocking until trusted policy records snapshot-current
  **Evidence** or an operator explicitly waives it; resolving a Finding never disposes of a
  Demand implicitly.
- Every prior Finding assigned to a required reviewer receives an explicit current-subject
  **Report**, **Dispute**, or **Drop**; absence is incomplete work, never evidence of repair.
- A **Change Attestation** or integrated Proposal moves covered claims to
  `pending-verification`; only a positive **Fix Verification** can produce a `fixed` resolution.
- A `rejected` or `wontfix-tracked` **Resolution** remains terminal only inside its recorded
  evidence, Subject scope, severity ceiling, and expiry; a material challenge moves it to
  `contested` through an explicit event.
- Every executable node resolves one **Execution Binding** before dispatch; safe Bindings expose
  revocable **Broker Handles**, never reusable credential bytes.
- A configured **Provider** dispatches only after current **Provider Admission**. Continuations
  name the exact Provider Operation epoch; stale epochs and repeated failure fingerprints are
  fenced before another external call.
- **Convergence** reads only the Round's exact final Finding Set and Demand Set plus recorded graph,
  gate, closure, and budget state; it never queries ambient latest projections.
- A **Subject** is always anchored to one head **Snapshot**; a `diff` Subject additionally
  names a **Base** Snapshot and derives a **Change Set** from the pair.
- Snapshot materialization atomically publishes CAS bytes only after fixed-buffer verification,
  then clones duplicate regular files in a separate bounded phase; candidate-controlled object
  size never becomes resident allocation or a reason to park the shared infrastructure executor.
  See
  [ADR-0020](docs/adr/0020-stream-cas-materialization-and-clone-duplicates.md).
- A **Proposal** applies only to its **Proposal Base**, which is the Subject's head Snapshot for
  that Attempt and is not the diff Subject's Base.
- A **Review Selector** resolves the Authority Snapshot and optional Base once per Campaign, then
  resolves each Round's head Snapshot into that Round's immutable **Subject**. Its labels may be
  retained as metadata but never participate in identity.
- A Campaign pins one **Authority Snapshot** and one **Campaign Manifest**. Later Rounds
  may advance the Subject's head Snapshot but cannot silently change pipeline or reviewer
  authority.
- The **Ledger** is a function of the event log and its referenced immutable artifacts. Delete
  the projection, replay those sources, same answer; a missing referenced artifact is corruption,
  not an empty field.
- **Finding Sets** and **Demand Sets** are immutable views produced by deterministic reducers;
  graph edges deliver exact view IDs and nodes never consume ambient "latest" state.
- **Report Scope** is derived for each **Report** claim from its exact Round Subject. Convergence
  evaluates active claims independently: a Finding blocks when any active claim is `in` at the
  severity gate and is wholly `out` only when all active claims are `out`. **News** and Report
  Scope remain independent axes.

## Example dialogue

> **Dev:** "Round 3 is reviewing a different change than round 1 — someone pushed to main.
> Should I re-run against the new base?"
>
> **Domain expert:** "No — you're describing a **Review Selector** moving, not a **Base**
> changing. The Base was pinned by Snapshot ID when the **Campaign** opened, so every
> **Round** measures the same delta and convergence reflects your fixes and nothing else. If
> you genuinely want to review against newer upstream, that's a new Campaign, because the
> **Findings** in this one are keyed to a **Subject** that no longer exists."

## Flagged ambiguities

- **"the change"** — used in both reviewer prompts and `heavy.toml` while the kernel captured
  only a whole-tree Snapshot, so it named something structurally unavailable. Resolved: the
  umbrella term is **Subject**, and "the change" is legitimate only under a `diff` Subject.
- **Review Target** — the original design used it for the immutable base/candidate/change-set
  tuple, while a later draft used it for mutable branch/PR/base-ref labels. Resolved: retire the
  overloaded term; **Subject** is the immutable reviewed object and **Review Selector** names the
  mutable resolution inputs.
- **Report vs Finding** — used interchangeably in prose. Resolved above: a Report is one
  attempt's immutable claim, a Finding is the triage identity many Reports attach to.
