# ADR-0275: Install a durable GitHub Scenario adapter

**Status:** accepted (2026-11-02). Implements the separately installed adapter boundary of
[ADR-0274](0274-orchestrate-pinned-scenarios-and-admit-ant-occurrences.md); it changes no Scenario
wire shape, Task delivery authority, Remote Check or signed Decision boundary.

## Context

A disposable local service fixture demonstrates the Scenario protocol but cannot publish a real
GitHub PR. GitHub creation accepts a head branch name, without an expected-head-SHA precondition.
It also supplies no generic idempotency key for PR, issue or review-request creation. A timeout
therefore cannot authorize a retry, and preflight HEAD lookup alone cannot protect exact approval.

## Decision

1. Ship an optional, single-file stdlib Python adapter and an offline owner-side installer. The
   installed executable contains all application code and pins its canonical external config
   path and config-byte digest. Existing Scenario machine policy pins the whole executable.
   System Python isolated mode with site imports disabled and stdlib are trusted runtime
   dependencies. No downloaded dependency, workflow executable path, URL, token or environment
   setting supplies authority.
   Owner-only external config specifies one GitHub.com repository and numeric ID, bot user,
   credential file, new private journal, exact head/base mappings, required CI identities,
   allowed reviewer identities and operations. Tokens are not part of configuration identity;
   every invocation verifies their actor. HTTPS is fixed to `api.github.com`, without proxies
   or redirects. Installation creates no credentials, service effects, Ants or schedules.
2. Preserve the existing stdin protocol and strict input ports. Typed JSON in Action `body`
   carries `af/github-publish-pr@1`, `af/github-followup@1` or `af/github-notify@1`. Each subject
   hashes the entire canonical operation payload, separately binding follow-up content and
   notification target/recipients. Revision is the approved exact commit SHA. Publish/follow-up
   retain kernel-verified human Decisions. Notification may also require its own Decision. Its
   fixed GraphQL mutation uses verified PR and user node IDs with additive reviewer union;
   workflows supply no GraphQL text.
3. Publish only from an administrator-provisioned same-repository head whose server protection
   enables `lock_branch` and `enforce_admins`, disables deletion, force pushes and fork syncing,
   and retains the approved SHA. Check protection/head before and after creation/recovery.
   Repository administrators preserving those protections are an explicit trusted boundary;
   the adapter never changes them. Unsupported protection is refused. A GET followed by POST
   without that server lock is not considered an atomic precondition. No push, ref mutation,
   merge, generic comment, email, fork head or arbitrary remote is implemented.
4. Retain a checksummed, versioned `af/github-adapter-state@1` journal, fsynced atomic writes and
   a stable nonblocking exclusive lock. Bind exact intent, entire request, opaque input digest
   and configuration identity. Persist dispatch **before** every potentially mutating call.
   Done requires validated actor, object, content and revision evidence, persisted before
   response. Only a locally proven never-dispatched intent can reconcile absent. Empty search,
   HTTP failure, timeout and crash never prove absence after dispatch. Repeated execute then
   reconciles; it never resends. Corrupt, foreign or missing state fails closed. A definitive
   pre-dispatch failure is durable and may allow a new bounded intent.
5. Reconcile PR/issue effects using exact marker, actor, approved content, repository and object
   identity, then direct readback. Keep irreducible ambiguity pending. A lost review-request
   response cannot be attributed through current reviewers, because removal can erase the
   observable effect; retain pending and never resend it. Follow-up creates one issue referring
   through its payload to a completed exact-head publication. A recorded publication's stable
   subject resolves its numeric PR identity for later Waits without changing the Scenario pin.
6. Observe exact SHA, trusted check app/name or status creator/context, newest attempts and
   complete bounded listings. Checks require the newest suite per app and explicit completed
   success for suite and run. This conservatively limits multiple workflows sharing an app.
   Missing, pending, failing, neutral, skipped, stale or incomplete evidence is never green.
   Bracket observations with PR identity/head reads. Merged requires the recorded exact PR/head,
   closed merged state, timestamp and merge SHA; closed-unmerged fails. Neither fact implies
   Task acceptance or authorizes merge.
7. Use fixed application bounds within existing 30-second supervision: 24-second wall, five-second
   requests, 100 calls, 256 KiB input/config, 2 MiB responses, ten 100-object pages, 4096 retained
   receipts and a 16 MiB journal plus one bounded scratch slot. Never expire uncertain receipts
   to reclaim capacity. Schemas, readers/writers and structural parity tests ship together.
   The fast gate runs credential-free mocked transport tests and a real installed-adapter
   Scenario CLI sequence. Live GitHub validation is not claimed.

## Considered options

- Preflight SHA GET plus ordinary create on an unlocked branch: rejected because the service
  mutation has no expected-head precondition and could publish a different revision.
- Retry after eventually consistent search returns empty: rejected because a prior effect can
  exist while invisible to that bounded search.
- Dispatch review requests by mutable usernames: rejected because lookup cannot atomically
  protect the recipient against rename and reuse. The fixed mutation uses stable node IDs.
- Infer review-request absence after reviewer removal: rejected because it loses invocation
  identity and can repeat a notification the service already delivered.
- Load application modules or credentials from a checkout/environment: rejected because the
  executable pin would not bind complete application authority.
- Add new Scenario ports or change core approval semantics: unnecessary; deterministic subjects,
  existing body text and local durable publication receipts compose the current protocol.

## Consequences

Operators separately provision protected heads, private credentials and explicit machine policy,
review the exact pin, and sign each required Decision. Unknown effects can remain pending and
require manual service inspection; safety does not depend on inventing authoritative absence.
Token rotation preserves actor identity; config/scope changes require a new reviewed installation.
Examples are inert. The complete contracts, installation, recovery procedure and limitations are
in the [operator guide](../scenario-github-adapter.md).
