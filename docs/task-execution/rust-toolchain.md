# Native Task Rust toolchain snapshots

Captured code policy can opt named checks into a private snapshot of pinned installed Rust.
This is not a shared writable RUSTUP_HOME, build cache, or new sandbox provider. Existing
trusted_local checks still run as the operator: arbitrary same-UID absolute-path access is
not prevented. Supplied toolchain/Cargo/Rustup roots do not alias host files, contain no host
Cargo credentials/configuration, and never write back to the reusable seed.

## Trusted preparation

Use a trusted installed Rust toolchain containing the required components. Prepare an absent
seed directory outside candidate trees with the bounded native helper:

    cargo run --locked -p review-sandbox --example toolchain-snapshot -- /absolute/installed/toolchain /absolute/private/seed

Retain the printed content pin. The helper copies at most 2 GiB/100,000 entries, refusing links,
special files and credential/config-shaped paths. It does not access Cargo home, install Rust,
invoke a network client or update mappings. Normal copy failures remove the partial destination;
an abrupt process death can leave a partial directory, which is never admitted automatically.
Existing destinations are refused. When upgrading, prepare a new absent destination and deliberately update the mapping.
Never derive that mapping or its digest from candidate check output.

Create a regular operator-controlled TOML file outside source (replace the illustrative pin):

    version = 1
    [rust]
    version = "1.88.0"
    host = "x86_64-unknown-linux-gnu"
    components = ["clippy", "rustfmt"]
    source = "/absolute/private/seed"
    expected_digest = "sha256:<64 lowercase hexadecimal digits>"
    max_bytes = 1073741824
    max_entries = 100000
    max_copy_bytes = 1073741824

Select it for the AF coordinator, not a check script:

    AF_TASK_RUST_TOOLCHAIN_POLICY_FILE=/absolute/operator/rust-toolchain.toml af task run TASK_ID

No global configuration changes. The AF coordinator selects the mapping path once when constructing
the domain and injects it explicitly; the library does not read the mapping environment variable.
The mapping and seed must have symlink-free absolute paths, including ancestors (on macOS,
use `/private/tmp` rather than `/tmp`). Only kernel-owned candidate roots are canonicalized.
The mapping is bounded and opened without following links.
Candidate commands do not receive this variable, mapping path or source path. Source files are
opened descriptor-relative without following links, copied into the private runtime, then hashed
against the pin before execution. Copied rustc must report the requested release and host.
Entry/byte/plain-copy limits are explicit. Each run copies again: this avoids network downloads,
not all I/O. There is no candidate-to-seed publication path, shared write lock or automatic refresh.
Parallel checks have different runtime roots. Partial copies never dispatch or become cache hits.
The native check diagnostic retains the verified digest, verified release, requested host,
resolved host triple and copy method; cold fallback emits an explicit `materialization: cold` marker;
it includes no machine-local source path and is diagnostic evidence, not a new acceptance type.

## Captured project request

Afactory requests this for its native kernel check:

    [rust_toolchain]
    version = "1.88.0"
    host = "native"
    components = ["clippy", "rustfmt"]
    checks = ["kernel"]

An exact host triple is also accepted. The native selector resolves to the mapping matching the
Linux/macOS architecture; private rustc verifies the identity. Local smoke evidence is Linux-only;
macOS behavior still requires its platform gate. Candidate channel declarations must
agree. Requiring container isolation together with this native-only request is refused: no host
paths enter another provider.

Without a mapping, existing cold setup and network behavior remain unchanged. A selected but
missing, malformed, corrupt, oversized or mismatched mapping fails closed, never downloads as
a repair. On a hit, copied binaries come first on PATH; CARGO_HOME/RUSTUP_HOME are fresh private
directories. Inherited `.cargo/bin` remains available for extra tools such as cargo-nextest;
the private prefix shadows its Rust proxies. Mapped seed paths and `.rustup` tool directories
are removed from PATH. This is tool selection, not same-UID filesystem containment.
Cargo dependency downloads remain separate under existing check policy; this does
not claim all of make check is offline.

The pin proves supplied bytes, not an upstream signature. Acquisition and mapping ownership
remain operator responsibilities. Checks may mutate their own copies; no runtime changes update
the mapping or seed.

## Focused smoke

`native-toolchain-probe MAPPING VERSION HOST [REPOSITORY]` checks copied tool versions twice.
The optional repository path also runs the real `make preflight-check` and offline
`cargo nextest list --list-type binaries-only` on a dependency-free fixture, with offline Cargo.
The fixture avoids importing a host registry cache; it is not a full-workspace nextest run. This supplementary smoke is not native Task acceptance.
