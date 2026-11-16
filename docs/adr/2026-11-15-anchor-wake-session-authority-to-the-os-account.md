# ADR-2026-11-15-anchor-wake-session-authority-to-the-os-account: Anchor wake authority

**Status:** proposed (2026-11-15); candidate repairs for #963 and #964.

## Context

The original-session registry followed the invoking shell's XDG state root. Two legitimate
shell configurations could therefore bind the same original session to different Task Stores,
bypassing retained native uncertainty. Codex socket validation also protected only its immediate
private parent, which another UID could rename through a writable higher ancestor.

## Decision

On Linux, resolve the effective UID through the operating system account database and use that
account's home, independently of `HOME`, XDG settings and Task Store selectors. Keep the existing
bounded machine control Store at the fixed `.local/state/af/task/local/` identity below that home.
There is no locator, environment override, second registry or copied binding in production.
Absent-only directory creation under a nonblocking parent-directory lock admits the initial
identity. An existing incomplete/damaged authority refuses initialization instead of resetting
bindings. The lock adds no durable metadata. All grant, registration, Pi poll,
foreground/detached driver and native preflight checks resolve
this same authority. Missing or invalid authority on use refuses; no route use creates authority.
The Store retains its existing incarnation, serialized first binding, finite 4096-binding limit,
logical byte capacity, monotone uncertainty and directory publication barriers. No persisted shape
changes and no compatibility reader or adoption of earlier XDG registries are introduced.

Include this Store in the existing machine inventory regardless of the invoking state root,
deduplicated with ordinary Task Stores. Its database, seal key and other Store files count against
the selected machine storage budget. It owns no business Task: the existing retained Store remainder
cannot be collected to free space or erase uncertainty. Alternate-root sweeps cannot reclaim its
session bindings. Body expiry and compaction continue in the authoritative business Task Store.
If retained authority exhausts capacity, new work refuses rather than clearing a fence.

Tests replace only the account-home lookup through a debug-only fixture setting, absent from
production release builds. Common integration launchers and individual wake fixture loops supply
private account scopes. Cross-root tests deliberately keep that scope constant while varying
ordinary environment/storage selectors; no test reads or writes the operator's account state.
Unit storage layouts carry their own optional inventory path. The production account lookup does
not read caller environment variables or canonicalize a caller-supplied alias into authority.

Validate every ancestor of the exact Codex socket pathname at grant and every installation
revalidation, using the same trusted-owner and sticky-directory policy as executable ancestry.
The immediate socket parent additionally remains canonical, owner-owned and private. Socket leaves
may remain mode `0777`; their device/inode/owner, canonical path and Linux peer credentials remain
required. Trusted owner/root sticky ancestors protect trusted-owned children. Foreign owners,
non-sticky group/other writable directories, links and changed identities refuse before native
intent or reconnecting queue invocation. Hostile changes by the same UID remain outside the model.

## Consequences

Ordinary shell configuration cannot create a new original-session authority namespace. Hosts with
an unavailable, noncanonical or unsafe OS account home refuse automatic wake instead of selecting
another root. Linux support, finite polling/process limits and no-resend behavior remain unchanged.
The driver gains no Task execution, model, login, service or publication authority.

The accepted research Document
`sha256:15bd1605f2d342797b89d57c7f66fb19c67b55a6cd5527208ee2cb6ac630ae97`, from
source Snapshot `sha256:485b04b460b678c5f1d62470e1f43a6aa08404fab3c8997b79f7a6ea3ee57e70`,
recommends the shared durable outbox and conservative ambiguous-admission fence. These repairs
preserve that recommendation; they provide no live host certification. Claude live remains deferred.
