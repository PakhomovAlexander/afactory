# ADR-2026-11-16-bind-only-required-measurement-namespace-branches: Bind required namespace branches

**Status:** proposed (2026-11-16); scoped repairs for #972, #973 and evidence gap #974.

## Context

The private measurement account launcher reconstructed foreign-owned ancestors by listing and
binding every sibling. A safe root-owned 0711 ancestor permits traversal but refuses that listing.
Six thousand unrelated private sibling names also exceeded exec ARG_MAX before Bubblewrap started.
Neither condition changes the validity of the declared private account or measured executable.

## Considered options

- Change host ancestor permissions, trust overflow ownership or run without isolation: rejected.
  These change authority instead of constructing the private namespace.
- Truncate sibling mounts or raise an argument limit: rejected. Required inputs could disappear,
  and ambient contents would still determine launchability.
- Transport an enumerated inventory through a descriptor: rejected for this repair. Required
  branches can be constructed without listing siblings or adding descriptor lifetime obligations.
- Bind declared path branches and fixed system runtime trees: selected.

## Decision

Construct mount branches from the private account, cwd, declared HOME/XDG/TMP/source/provider paths,
PATH and command inputs. Preserve lexical symlinks and their canonical target branches. Bind the
system runtime trees explicitly for loaders, libraries, NSS, devices and subprocess tools. These
runtime trees retain their original ownership, including foreign executable leaves. Other foreign
ancestors use private directory containers with their original modes and sticky bits. Required
subtrees and leaves retain their backing inodes, bytes and filesystem capacity. No directory listing
or ambient sibling name contributes mount arguments. Missing required inputs still fail at use;
there is no alternate executable, account, privilege or unisolated execution path.

Namespace setup remains outside measured AF intervals. Actual AF argv, supervision, filesystem
budgets and counters remain unchanged. Contained execution retains its existing capability and
no-new-privileges boundary. Production ownership and account admission rules are unchanged.

The accepted research Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`, from
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`,
recommends durable shared subscriptions/outbox and conservative uncertain-admission handling.
This launcher repair preserves that recommendation and the cumulative wake implementation.
It grants no execution/publication authority or live host certification; Claude remains deferred.

## Consequences

The existing required e2e final-child isolation regression also runs real built-AF account inventory,
scripted-provider Task planning and the exact native missing-provider release smoke receipt across
ordinary, search-only and populated ancestors in both tmp-style and home-style private fixtures.
It runs as a non-root UID even on root CI hosts. The single-UID sandbox cannot create two mapped
owners: only launcher-side stat of each selected fixture ancestor models root ownership. Actual
0111 permissions deny listing by its non-root fixture owner; the modeled root mode is 0711.
Child AF ownership checks, NSS resolution and receipts use the real namespace without interception.
Existing external-account open tripwires and database/key/WAL/SHM equality assertions remain.
These are deterministic implementation regressions, not performance measurements or Pi/Codex live
certification. Linux namespace support remains required; there is no production fallback.
