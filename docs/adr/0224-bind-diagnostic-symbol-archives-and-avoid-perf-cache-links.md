# ADR-0224: Bind diagnostic symbol archives and avoid perf cache links

**Status:** proposed (2026-10-26). Extends
[ADR-0174](0174-diagnose-in-separate-child-runs-that-record-only-the-bracketed-region-and-share-one-test-executable-boundary.md),
[ADR-0175](0175-supervise-every-diagnostic-execution-admit-what-it-wrote-and-record-what-it-used.md)
and [ADR-0176](0176-diagnose-cli-cases-and-contained-linux-runs-and-bound-every-step-and-seed-before-it-is-used.md).

## Context

The #353 candidate reduced diagnostic build and symbol-retention bytes, but its `retained`
claim did not identify an archive in `profiles.json`. A manifest-valid bundle could omit the
archive or contain an unrelated executable. Review also lacked an accessible executed-build
report and a decision recording the diagnostic settings. Prior executed evidence exists; its
[retained report](../performance-evidence/diagnostic-symbols-353.md) states its identity,
source relationship and limitations. This continuation carries no inherited acceptance.

Coordinator validation for #357 recorded capability samples, then perf automatically created a
build-ID-cache hard link to an external admitted executable inside private HOME. The existing
cleanup guard correctly refused cleanup and retained scratch. There were no usable captures or
flamegraphs. Changing cleanup authority would conceal the failure.

## Decision

1. GNU CPU diagnostic recipes freeze `-Cdebuginfo=line-tables-only` in recipe rustflags.
   `debug_symbols` stays true and frame pointers stay enabled. Function names, source lines,
   inlined locations and unwind information remain available; full local-variable/type debug
   information is not promised. Ordinary verdict recipes, Cargo/release settings and runtime
   code are unchanged.
2. Retain exact, unstripped executables as deterministic XZ (preset 6, CRC64, no filename or
   timestamp). The path `symbols/<binary-sha256>.xz` identifies the original executable; the
   archive's SHA-256 identifies its compressed bytes. Runtime symbolization uses the admitted
   original executable. Compression checks the existing deadline between bounded chunks and
   writes only an archive that fits the remaining room, charging its actual bytes. Identical
   binaries share one archive; distinct binaries pay separately. Other binaries remain
   `referenced`, or `missing` when unavailable.
3. Under [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md),
   `af.perf-profiles/6` adds required nullable `symbols.archive` (path, compressed bytes and
   SHA-256). Exactly `retained` requires this record and a non-null binary identity. The path
   must be the binary's content address; shared references must agree exactly. `referenced`
   and `missing` require null. Versions 1–5 are refused, with no compatibility reader.
   Writers, readers, authoritative schema, current goldens and parity tests change together;
   the telemetry fields and the report contract keep their existing meanings.
4. Finished collection and report admission require each retained archive to be a verified
   bundle member with the declared compressed size and hash. They stream one XZ stream through
   confined descriptors and require the decompressed SHA-256 to equal `binary_sha256`.
   Compressed members fit the existing diagnostic cap (at most 512 MiB), each also fits the
   child's declared cap. All distinct decompressed originals together fit the existing 4-GiB
   build-artifact maximum. Decoder memory is limited to 64 MiB, input chunks to 64 KiB, output
   chunks to 1 MiB, and all archives share one 600-second admission deadline, tightened by any
   enclosing collection deadline. Missing, unclaimed, corrupt, truncated, concatenated,
   trailing (including stream padding), oversized or falsely addressed evidence refuses.
   Shared captures decode once per admission and never charge another physical archive.
   Normal manifest integrity remains an independent compressed-byte check.
5. Both acknowledged-region and whole-command `perf record` pass `--no-buildid-cache`.
   The capability probe uses the same region argv builder. The option avoids automatic cache
   population while retaining build IDs in `perf.data`, as the
   [upstream Linux perf-record documentation](https://github.com/torvalds/linux/blob/v6.12/tools/perf/Documentation/perf-record.txt)
   specifies. `--no-buildid` would discard those IDs and is not used. No global configuration,
   event, frequency, unwind, control, oracle or cleanup rule changes. Hard-link, alias and
   symlink checks still fail closed; uncertain scratch stays preserved.
6. Build capacity and symbol capacity are separate proofs. The inherited executed proof built
   both independent source sides and the supervisor below the unchanged 1-GiB selected build
   cap. Its AF pair and probe pair separately fit the 128-MiB evidence cap with its 8-MiB
   administrative reserve. All four archives total 141,891,932 bytes and cannot all fit that
   allowance. Mixed selections may therefore record references. Neither proof establishes an
   end-to-end profile within its deadline or qualifies a speed claim.

## Alternatives and limits

Full debug information exceeded the selected artifact budget; stripping or reducing build
coverage would discard needed evidence. A retained flag without a verified member is
unverifiable. Trusting only the manifest hash establishes compressed integrity but cannot bind
its content to the binary. Permissive old-version admission would violate ADR-0145.

Disabling build IDs or relaxing cleanup would lose attribution or authority. Explicitly
avoiding perf's cache addresses the demonstrated side effect while preserving both. Behavioral
fixtures reproduce the hard link and exercise unchanged cleanup; they are not actual perf
capture, symbolization or flamegraph qualification. Fresh coordinator proof with pinned tools,
the unchanged full native/Markdown gate and independent review/evaluation remain obligations.
The original failed coordinator scratch and evidence are outside this sandbox and untouched.
