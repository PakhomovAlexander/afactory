# ADR-2026-11-06-publish-verified-performance-under-standing-owner-authority: Scoped performance PRs

**Status:** accepted (2026-11-06). Supersedes ADR-0274 clause 4 and
ADR-2026-11-03-native-performance-scenario-loops' exact Decision requirement only for the new
`publish_performance_pr_v1` operation. Generic `publish_pr` and `followup` retain their exact
signed human Decisions. Task delivery and Remote Checks are unchanged.

## Context

The owner authorizes automatic normal PR publication for accepted Performance Scenario/Ant
optimization loops, including the deterministic candidate commit and creation of a new scoped
branch. Human merge remains required. The existing installed workspace and GitHub adapters
already retain intents, native source provenance, exact ref preconditions and uncertain HTTP
receipts. They provide the bounded implementation rather than a second publication runtime.

## Considered options

- Sign an artificial Decision or retain a reusable signing key: rejected. Model text and agent
  credentials cannot manufacture human authority.
- Broaden `publish_pr` or Task delivery: rejected. Their existing contracts remain useful and
  their authority must remain separate.
- Trust a supplied JSON verdict, successful process or verification boolean: rejected. Native
  Task Stores and retained, independently verified native experiment evidence own those facts.
- Retry a lost push or POST blindly: rejected. Reconciliation must recover the original intent.

## Decision

1. `publish_performance_pr_v1` is an explicit opt-in operation. The external owner-only machine
   policy pins its installed adapter and a standing scope: exact repository name and numeric
   GitHub identity, native repository and Task Store, base, source commit/content, branch namespace
   and native evidence adapter/destination. The Scenario grants only a subset. Installation pins
   the same scope and trusted executable/configuration bytes. An Ant alone grants nothing.
2. Before publication, both the native runtime and installed workspace adapter re-open exact
   accepted implementation, independent read-only verification and final report Tasks. Native
   model bindings include the required reviewers and evaluator. Verification retains exactly its
   input Snapshot and references the implementation's exact result. The accepted report consumes
   that verification's source and the exact comparison. Unaccepted or unrelated artifacts refuse.
3. The native lab adapter is independently pinned and re-observed. Its retained offline-verified
   core comparison must match the implementation's original source and exact candidate. Only
   measured, complete, passing local bootstrap/partial comparisons with a non-exploratory native
   improvement and no failing or inconclusive case qualify. Telemetry, diagnostic, regression,
   incomplete, unsupported and merely within-tolerance comparisons refuse. This local evidence
   does not satisfy a remote required Performance check.
4. The installed adapter checks local and remote base equality before commit and publication.
   It uses existing filter-free native source export, deterministic `commit-tree`, owned detached
   workspace reservation and bounded journals. Only a new branch below the pinned namespace may
   be pushed. An empty expected-ref lease atomically requires absence at receive-pack; it cannot
   force-update an existing ref, even to a descendant. Existing branches, foreign refs, merges,
   remote gate policy and Task delivery receive no authority. Candidate `.github/` changes refuse.
5. The default is a normal open PR with an unmodifiable head. Its body contains the actual native
   `af task report` block for implementation, independent verification and final report, the
   accepted final report and exact measured comparison including before/after values, interval,
   scope and native identities. A title supplies no evidence or authority.
6. Local preparation and GitHub intents retain the same stable idempotency key. Deterministic
   commit recovery recreates the same object. A lost push reconciles the exact owned ref before
   another push; an existing matching ref needs none. A possibly dispatched POST never becomes
   absent from an empty listing and is never repeated. Retained markers recover its single PR.
   Drift and uncertainty retain artifacts for inspection instead of authorizing another effect.
7. Changed closed contracts are Scenario/Policy/Bundle/Pin/Run/List and Ant/Pin/State/List/Tick v5,
   Scenario Store v6, adapter request v4, workspace config/state v2 and GitHub state v4. New
   performance proof, content, activation and GitHub payload contracts have their own v1 shapes.
   Writers, strict readers, schemas and parity tests change together; old shapes fail closed.

## Consequences

The inert authoring tool can add the operation to a candidate loop and its disabled Ant only when
an operator supplies reviewed installation and scope receipts. The no-worthwhile loop carries no
publication operation. No live installation, scheduler, benchmark, credential, signature or key
is supplied by this change. The [runbook](../performance-scenarios.md) explains preparation,
manual proof and later separate Ant activation. Generic signed publication remains unchanged.
