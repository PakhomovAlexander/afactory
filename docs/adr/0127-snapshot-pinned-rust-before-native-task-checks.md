# Snapshot pinned Rust before native Task checks

**Status:** accepted (2026-10-01)

## Context

Fresh native Task check homes cause rustup to download installed Rust repeatedly. The check
script cannot safely discover host state from its cleared environment. Preparation belongs to
the coordinator, before check dispatch, rather than candidate-selected host paths.

## Decision

An optional captured native code-policy request selects version, host, components and named
checks. An explicit machine-local mapping selects a real installed-toolchain subtree, content
pin and bounded copy limits. The coordinator opens no-follow descriptors, copies regular files
into each private runtime and verifies the resulting manifest digest plus rustc release/host.
Cargo credentials/configuration and host rustup settings are not copied. Hardlinks become new
private files. Candidate runtime writes are never promoted to the source or mapping.

Use the existing trusted-local provider only. This does not claim OS containment or prevent
arbitrary same-UID absolute-path access. A container requirement refuses this native-only request.
No request/mapping preserves existing cold behavior; an explicitly invalid mapping fails closed.
Toolchain preparation does not download anything. Cargo dependencies remain a separate concern.

## Considered options

- Host RUSTUP_HOME/CARGO_HOME passthrough: rejected for aliasing and credential exposure.
- Candidate script discovers/caches host tools: rejected because source selection and reusable
  admission proof would become candidate-owned authority.
- Persistent writable gate cache: rejected because later gates could inherit candidate mutations.
- Private copies of an explicitly pinned seed: chosen. Simpler than a new durable publication
  protocol; independent run roots eliminate shared writable publication and lock races.

## Consequences

Every hit pays bounded local copy/hash I/O instead of another download. Source acquisition and
pin ownership remain trusted operator responsibilities. No upstream signature is implied by the
content pin. Partial copies never dispatch and are not reused. Existing sandbox/process rules
remain in force; this is narrow native preparation, not a generic provider redesign.

The coordinator injects the machine mapping explicitly into the domain. Kernel-created candidate
roots are canonicalized for macOS temporary-directory aliases; operator mapping and seed paths
retain strict no-follow admission. Private Rust binaries prefix PATH without hiding installed
Cargo subcommands. Evidence distinguishes cold fallback from a copy and records the resolved host.

The dedicated `task_rust_toolchain` integration binary is a scoped exception to ADR-0124
required by this feature’s focused verification contract. Its PATH-specific child process uses
a module-aware exact filter and requires one passed test; domain mapping tests run in-process.
