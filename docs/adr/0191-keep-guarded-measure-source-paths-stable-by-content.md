# Keep guarded measure source paths stable by content

Status: accepted, 2026-10-21.

## Context

The guarded performance adapter builds a native probe for each kernel measurement repetition.
ADR-0161 refreshes the materialized Snapshot's timestamps before every native check. This
correctly prevents reuse of stale product builds, but makes the adapter rebuild identical
sources on every repetition. Integration validation observed 68–88 seconds of repeated build
setup, obscuring an injected 1.5-second operation improvement in the end-to-end objective.

## Decision

Build the guarded probe from one source copy inside the kernel-bound Cargo target. The kernel
already leases this target exclusively and accounts for it under the machine storage budget.
Before every build, compute the content digest of every regular file in both the measured
Snapshot and the copy, with the existing 200,000-entry and 2-GiB bounds. The digest's domain
advances to `af.perf-kernel-sources/3` and includes executable mode: build scripts can invoke
source helpers. No path is excluded by name. Links and special files fail closed. A remembered
digest alone never authorizes reuse.

When content matches, keep the copy's paths and timestamps. When the copy is missing or differs,
replace it and invalidate the workspace's own Cargo fingerprints, even if the last successful
build's source digest matches. Verify the completed copy before building. Always invoke the
offline, locked Cargo build; never execute a cached binary solely because a marker matches.
Dependencies remain warm. Failed or interrupted setup cannot record a successful build.

Only one copy is retained, rather than one directory per Snapshot. Copying preserves file
modes but gives changed files fresh timestamps; owned cache directories remain removable.
The kernel's original materialization is unchanged. Setup remains inside kernel elapsed time
and outside the probe's inner timed region. Existing objectives, thresholds, repetitions,
freshness rules for ordinary checks and receipt formats are unchanged.

## Validation

Adapter tests exercise fresh materializations with different timestamps, changed content under
a directory named `target`, a return to earlier bytes, a corrupted copy with an unchanged
successful-build marker, read-only source directories and symlink refusal. The native guarded
experiment test requires warm repetitions to retain the same probe bytes and independently
requires the declared end-to-end improvement; the inner metric alone cannot make it pass.
