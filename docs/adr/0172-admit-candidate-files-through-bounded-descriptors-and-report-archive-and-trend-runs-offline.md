# ADR-0172: Admit candidate files through bounded descriptors, observe streamed JSON by path, and report, archive and trend runs offline

**Status:** accepted (2026-10-20); decisions 1 and 7 amended by
[ADR-0173](0173-own-the-supervisor-a-test-runs-and-bound-admission-collection-scratch-and-cancellation-by-absolute-deadlines.md) (enumeration bounded while it reads, and one absolute deadline per HTTPS
exchange). Closes five inherited admission and oracle findings against
the sealed snapshot of
[ADR-0171](0171-cancel-samples-through-their-supervisor-carry-the-deadline-through-preflight-and-observe-history-order.md),
whose own acceptance was still pending when this package began, and implements package p4
(reports, archive and history) of the
[continuous performance laboratory](../design/continuous-performance.md). Its decisions stand
except where this ADR amends them:

- decision 3 (the run's deadline now also bounds the artifacts' admission);
- decision 4 (the `task show` rules), which this ADR replaces.

The new contract versions are:

| Contract | Version | What changed |
| --- | --- | --- |
| Supervisor request | `af.perf-supervisor-request/4` | `stdout_json` selectors replace prefixes |
| Supervisor result | `af.perf-supervisor-result/5` | their streamed JSON observation |
| CLI registry | `af.perf-cli/5` | `json_stream_*` rules replace `stdout_sequence` |
| Report | `af.perf-report/1` | new |
| History | `af.perf-history/1` | new |
| Archive, event, collection, status | `af.perf-archive/1`, `-event/1`, `-collection/1`, `-status/1` | new |
| Transport | `af.perf-transport/1` | new |

## Context

Root confirmed five findings against the sealed predecessor:

- **F2, artifact admission before the deadline.** `Shard.__init__` built `Binaries`, which
  rehashed every artifact, before it read the run's deadline. With 0.5 s left and each hash
  advancing a controlled clock 2 s, all five hashes ran, four after the caller's deadline, and
  only then did the shard refuse.
- **F3, the `task show` oracle did not read JSON.** The supervisor counted the number after a
  byte prefix. Against the actual supervisor, a middle number `5000.5` or `5000e1`, every entry
  but the first moved into an unrelated array, and a document without its closing brackets all
  passed. Equivalent whitespace and member order failed, because the retained identity checks
  froze compact formatting.
- **F4, sealing followed a seed's symlink.** `seeds.seal` ran `chmod` on every entry before
  `seeds.tree` refused a symlink, so a candidate seed's symlink changed an owned file outside it
  from 0600 to 0555.
- **F5, seed reads were unbounded.** `seeds.tree` hashed whatever logical size a file had. A
  sparse file of 2,147,483,649 logical bytes and no allocated block was admitted. Nothing
  bounded entries or time.
- **F6, build publication followed a candidate symlink.** `Builder._publish` copied a candidate
  output that was a symlink to an owned file outside the target, so the file's bytes became a
  retained artifact. It also published an AF binary whose embedded commit was not found.

Package p4 then needs offline reports, an archive that outlives CI artifacts, and compatible
history, under the design's boundaries: no executed or trusted downloaded code, no external
script or URL, no invented sensitivity, and no history that mixes identities.

## Considered options

1. **Refuse a symlink after sealing, or check `islink` before each `chmod`.** Rejected: that is
   F4, and a check followed by a path operation still races a same-user writer.
2. **Bound seeds by the scratch quota alone.** Rejected: that is F5. A quota bounds allocated
   blocks, not the logical bytes a hash reads.
3. **One shared admission module.** Chosen. Every candidate-written tree is enumerated through
   held directory descriptors with `O_NOFOLLOW` at every component. Its kinds, entry count,
   depth and logical bytes are checked before anything is read or changed. Modes change through
   each entry's own verified descriptor, and every read is bounded by the enumerated size and
   the dominant deadline.
4. **Count more prefixes, or digest the whole output.** Rejected: neither knows JSON paths,
   membership or completeness, and a digest freezes formatting.
5. **Parse the whole output on the host.** Rejected: the 1 MiB log cap is fixed, and the host
   never sees the unretained bytes.
6. **A streaming JSON observer in the supervision leaf.** Chosen. It is dependency-neutral and
   observes the whole stdout one byte at a time with memory bounded by depth and selectors.
7. **A database or hosted dashboard for history.** Rejected by the design: a filesystem archive
   and offline pages suffice, and other backends can read the format later.
8. **Render a transported report.** Rejected: transported HTML is artifact data, and its CSP
   hashes are no authority.

## Decision

1. **Candidate files are admitted through bounded descriptors** (`perflab.admission`).
   - **Enumeration first.** A seed tree is scanned whole before any mode or byte is touched. A
     symlink, a symlinked directory, a FIFO, a socket or a device refuses it. So do more than
     100,000 entries (`SEED_ENTRIES_MAX`; the largest declared seed holds about 20,500) or 64
     levels, or more logical bytes, per file or together, than the environment's
     `scratch_limit_bytes`, which every sample's copy must fit.
   - **Confined change.** Sealing opens each entry again one component at a time, without
     following, and changes its mode only if it is the very entry enumerated. Removal unlinks a
     symlink and never follows it.
   - **Bounded reads.** Every hash reads exactly the enumerated size in 1 MiB chunks and checks
     the dominant deadline before each one. A file that grows or shrinks refuses.
   - **Ordering.** A writer whose end is not confirmed has nothing of its seed read, sealed or
     removed. The shard keeps the seed and stops, as ADR-0171 decided for scratch. A deadline
     that ends an admission interrupts the shard as the deadline's.
   - **Identity.** A successful seed's digest is computed exactly as ADR-0169 defined it, so
     existing identities are unchanged.
2. **The run's deadline precedes artifact admission.**
   - **Order.** A shard reads `builds.json` within the JSON bound, checks that it names this run
     and plan, and fixes the run's monotonic deadline. Only then does it rehash the artifacts.
   - **During admission.** Bundle enumeration and every hash chunk leave the 30 s terminal
     reserve. Nothing starts once the reserve is reached, and an expiry mid-way refuses the shard
     as `run_deadline`.
   - **Sizes first.** A bundle's manifest sizes are admitted against the bound and every file's
     logical size before any hash read.
   - **Later rehashes.** Each binary rehash before a launch and each seed check use the shard's
     deadline the same way.
3. **Build outputs are published only when admitted.**
   - **Confined open.** An output is opened below the run's own worktree, one component at a
     time without following, so a symlinked file or ancestor refuses before any byte is read.
   - **Budgets.** Its logical size must fit what remains of the artifact budget.
   - **One read.** A single read within the run's deadline hashes it, copies it with
     `O_EXCL|O_NOFOLLOW` and finds an AF binary's embedded commit.
   - **Refusal.** A refused output, or an AF binary without its commit, is never published. The
     side fails and the reason is appended to the step's retained log, which `builds.json` binds
     by hash. A refused probe output fails the side without trying another adapter.
4. **The supervisor observes declared JSON fields** (`review_process::json_stream`).
   - **The request.** It may name up to eight selectors: a dotted path of member names and `*`
     from the root, and a kind.
   - **The document.** The whole stdout must be exactly one complete RFC 8259 text: UTF-8, every
     token's grammar, nesting at most 512 levels deep, and nothing after the value but
     whitespace. The result says `complete`, `malformed`, `incomplete` or `too_deep`, with the
     offset where it failed.
   - **Per selector.** It reports the values the parent path reached, the path's own values,
     repeated selected member names and values of another kind. An integer selector accepts
     only plain nonnegative integers that fit 64 bits and reports its first, last and breaks. A
     string selector counts each declared value after unescaping.
   - **The rules.** `json_stream_equals`, `json_stream_sequence` and `json_stream_count` require
     a complete document. `task show` now requires `task_id` to be exactly the fixture Task's.
     Every `history` entry must hold one `sequence` numbered 0..9999 in order, and exactly 9,997
     `history.*.transition.change.kind` values must be `lease_renewed`. A seed's sequence still
     counts its declared events.
5. **Reports** (`report --run DIR --output DIR`).
   - **Inputs.** The verdict is recomputed by `compare.evaluate`, which now also returns the
     evidence it verified.
   - **Outputs.** A new directory receives a self-contained page, the matching
     `af.perf-report/1`, the verdict, a GitHub job-summary table and verified copies of the
     bundle members it links.
   - **The page.** Every evidence string is escaped. The CSP allows only the bundled script and
     style by their SHA-256 hashes and local images, and links name only validated relative
     paths of files the report wrote.
   - **Raw points.** Every planned slot is a point, warmups, failures, sentinels and slots
     without an admitted record included, beside the verdict's exact rational bounds,
     classifications, budget observations and side-specific failures.
   - **The required check.** The report states every reason a run does not satisfy it: the
     result, the scope, the authority mode or the evidence. It keeps measured results apart
     from calibrated sensitivity, never infers a per-run minimum detectable effect, and marks
     profiles and traces unavailable until package p5.
6. **The archive** (`archive --root DIR`) is content-addressed and append-only.
   - **Writes.** An entry is written to staging, synced and linked to its next sequence name,
     so an interrupted command leaves only staging, which the next one clears.
   - **Admission.** A run is admitted only whole, and a partial or tampered bundle refuses.
     Members are rehashed while copied, and an existing object is rehashed before it is trusted.
     Re-importing a run is idempotent, and another run under its ID refuses.
   - **Capacity.** The default is 100 GiB and the maximum 1 TiB. It and the filesystem's free
     space are checked before anything is written, and exhaustion is a recorded collection and
     a health failure.
   - **Retention.** Numeric evidence, plans, build manifests, verdicts and summaries stay 365
     days, and logs, transported views, profiles and symbols 90. A pinned run stays whole until
     superseded and then 365 days more.
   - **Expiry and collection.** Expiry writes a retention entry with the original manifest and
     what remains. Garbage collection removes only objects no available member references.
   - **Status.** `--status` reports capacity, usage, collection lag, the last successful
     collection, transport expiry risk and health, and exits 4 when unhealthy.
7. **The collector** (`archive --fetch ACTIONS_RUN_ID --repository OWNER/NAME`).
   - **Credentials.** Its token comes from `--token-env` and is sent only to the configured API
     origin. API paths are built from the repository and validated numeric IDs. The download's
     redirect to signed storage is followed by its own handling, without any credential, and
     signed locations are never recorded.
   - **Bounds.** Pages, redirect hops, response bytes, request and collection time, ZIP members,
     each member's and all decompressed bytes and the compression ratio are all bounded.
   - **The format.** Encrypted, symlinked, duplicate, absolute or traversing members refuse. An
     artifact must declare `af.perf-transport/1`: one run or shard bundle and optional views.
     Expired or missing artifacts leave the retained run incomplete, which its verdict and the
     archive's status show.
   - **Benchmarks.** No benchmark receives the token.
8. **History** (`history --inputs DIR... --output DIR`).
   - **Inputs.** Run directories and archive roots. An archived run is rebuilt from its objects,
     or shown from its archived summary with what expired.
   - **Groups.** A group shares the case, workload declaration, the authority's fixture
     definitions, policy, recipe, environment and provenance. Run IDs, sources and seed artifact
     digests are provenance on each point and never split a group.
   - **The page.** Each run links its own report by the same renderer, and the page says that a
     trend is not a same-host paired result. Transported views are opaque `.bin` downloads.

## Consequences

- **Versions.** Earlier supervisor request, result and CLI registry versions are refused with
  no reader. Their schemas are replaced by `performance-supervisor-request-v4.json`,
  `performance-supervisor-result-v5.json` and `performance-cli-v5.json`. Every new contract
  ships its schema and valid and invalid fixtures.
- **Unchanged bounds.** The 15-second production lease, the 8-MiB envelope cap, every sample
  timeout, log cap, pair count, the 2-GiB scratch, the run deadlines and the terminal reserve are
  unchanged. Only new bounds were added: seed entries and depth, ZIP and transport limits, and
  the archive capacity the design fixed.
- **Real-process tests.** These run against the actual supervisor over synthetic streams:
  - fraction, exponent, moved entries and an unclosed document fail;
  - equivalent whitespace and reordered members pass;
  - every chunk boundary is covered.
  The actual `af task show` over a side-written 10,000-event seed passes the new oracle. The leaf
  unit tests cover escapes, surrogates, depth, duplicates and declared bounds.
- **Python tests.** They cover:
  - the actual shard constructor with controlled clocks: 0.5 s left starts no hash, and three
    of five 2-s hashes fit 35 s;
  - owned outside marker files and directories unchanged after symlinked seeds, both in
    `seeds` and through a real shard;
  - sparse 2-GiB+1 files refused without reads;
  - entry, depth and deadline bounds;
  - hostile build outputs: a symlinked file, a symlinked release directory and a sparse 4-GiB
    binary;
  - report escaping, CSP, links, denominators and required-check honesty;
  - archive idempotency, collisions, partial and symlinked bundles, interrupted imports, gaps
    and unknown versions, capacity, retention, pins and shared-object garbage collection;
  - the collector against a fake transport with a marker token;
  - history grouping over the actual runner's shards.
- **Not evidence.** Nothing here is calibrated performance, a hosted run, an unattended
  collection or an external archive deployment. The collector's runbook,
  [performance-archive.md](../performance-archive.md), names what an operator must provision and
  observe before claiming continuous collection. The transport format is this package's; package
  p6a's workflows must upload it.
