# Bounded Worker knowledge

This package is a deterministic example for any source language. The kernel uses captured
source identities and ordinary typed ports; Python is only the example command harness.
It can be combined with the [Rust and Python development environments](../worker-development/README.md)
without a second runtime or shared writable state. The
[design](../../docs/design/worker-local-development.md#step-3-bounded-knowledge-retrieval-and-private-notes)
and [decision](../../docs/adr/2026-11-04-select-bounded-worker-knowledge-through-typed-ports.md)
state the complete contract.

## Configure a package

Copy the package files into a consuming project's Worker package. Review and pin that package
through its normal Execution Binding. `knowledge.toml` is included in the existing digest;
changing it requires a new package pin and ordinary exact-plan approval. There is no machine
path, network permission or credential grant in the knowledge policy.

The provided policy selects a range of `${source}/guide.txt` and declares `config.txt` as an
additional dependency. Use canonical Manifest path spelling, not a host path. The index hashes
both files' contents and kind, and distinguishes missing from empty dependencies. Change these
explicit paths and ranges for your project. A missing source file is an explicit omission;
a missing dependency remains an explicit absence in applicability evidence.

The six limits cap manifest bytes, total knowledge-context JSON bytes, retrieved UTF-8 bytes,
retrieved items, each source file's bytes and the private Notes-plus-Head-Delta bytes. A file
larger than `source_bytes` is omitted; a range does not authorize reading an unlimited file.
Use byte boundaries that preserve UTF-8. Unreadable, symlink, non-text, credential-shaped and
oversized inputs have omission reasons. Metadata alone must fit the manifest/context bounds.
Notes' existing exact rename diff additionally admits at most 16,384 combined input entries
and 256 MiB across the two Snapshots; an oversized input or delta omits private knowledge.

`private_notes = "same_slot"` uses existing Notes ports. The kernel supplies producer/source
identity and checks the effective slot. The example deliberately leaves a private hypothesis
whose bytes never appear in the next invocation unless that invocation explicitly queries
`private_notes`. Notes remain hypotheses even on unchanged source. `freshness = "mark"` marks
changed Notes for revalidation; `omit_stale` drops them. The fixture never asks for its Notes.
No raw session, HOME, mutable knowledge directory or failed Attempt state is imported.

## Wire discovery and retrieval

The package declares the optional unbound kernel input `knowledge`. Leave it unbound in every
Pipeline. The first invocation receives a bounded manifest and no excerpts. Its
`knowledge_query` output names the index and `guide` entry ID from that manifest. A later node
on the same slot binds that output:

```toml
[[nodes]]
id = "discover"
operator = { op = "worker", slot = "author" }
[nodes.inputs]
source = { kind = "input", port = "source" }

[[nodes]]
id = "read"
operator = { op = "worker", slot = "author" }
[nodes.inputs]
source = { kind = "input", port = "source" }
knowledge_query = { kind = "node", node = "discover", port = "knowledge_query" }
```

Place these nodes in a Pipeline with explicitly typed source/output contracts and the declared
`author` slot. Its accepted Task kind supplies its real Gate and receipt/acceptance policy;
the example's `passed` document is only a transport fixture. The test captures this exact
Worker package in a two-node Pipeline and supplies that fixture kind's policy separately.
Independent reviewers use their own slots, sources and declarations. Never wire Notes across
slots: the compiler refuses it. A retry carries its pinned selection, never failed-sibling Notes.

Retrieval happens between explicit Worker invocations through existing typed ports. There is
no live tool callback or hidden model call. A Worker can query different subsets on later
nodes by emitting a fresh query from each delivered manifest. An imported query that names
stale entries records `stale`; it does not retrieve a different file at the same name.

For native model Workers, replace only the runner, assign an adequate nonzero token reservation
and use an admitted Provider binding. For example:

```toml
[runner]
kind = "model"
provider_kind = "codex"
model = "gpt-6.1-sol"
effort = "medium"
```

Use instructions that select entries and return the declared business result. The native
adapter receives the same exact knowledge context as the command harness. Its sandbox and
credential boundaries stay those of the existing Provider adapter. The step-1 development
companion still requires its supported container command harness and machine binding; adding
knowledge does not enable native model development environments.

## Cross-Task facts and evidence

Keep `cross_task = "disabled"` for ordinary same-slot work. For intentional reuse, declare
`cross_task = "source_facts"` and an optional typed Task input of `af/KnowledgeQuery@1`, bound
to a recorded admitted predecessor output through the existing Task input-binding mechanism.
Wire that root port to the receiving node's `knowledge_query`. The receiver must declare the
same source range and dependencies: only entry identities revalidated against its current
captured source are eligible. This capability also governs another slot's fact query.
Private entries remain refused, even if an imported index contains them. There is no ambient
cross-Task index search and no trust transfer from another implementer's conclusions.

Inspect the Attempt's context evidence for `knowledge-index`, `knowledge-warm` and
`knowledge-excerpt:NAME`. Each excerpt names its exact UTF-8 blob; the knowledge input contains
source artifact/Snapshot/path/range/dependency IDs and supporting evidence. The context's
rendered object records the exact JSON request. Step-1 command evidence already names that
context. Raw Notes/query inputs have zero delivered bytes and remain provenance only.

A command harness may use a model internally; the host sees only its outer request/output.
It cannot observe or claim the harness's internal prompts, file reads or model consumption.
The immutable CAS index is rebuildable from captured source and Notes records. Existing Task
leases, retention, machine Storage Budget and collection own all artifacts. Bounds and
missing/evicted/corrupt material produce explicit omissions; an unavailable recorded context
refuses dispatch instead of silently selecting new memory. Neither knowledge nor caches
establish Gate success or independent acceptance.

## Deterministic checks

`task_runtime::knowledge` captures these package files and runs both command and model Tasks
through the real host, retry, admission and replay path. A deliberately rejected model reply
cannot contribute Notes to its retry. `task::knowledge::tests` additionally runs the command
transport and native Codex adapter with an exact-path fake CLI, and covers isolation, freshness,
bounds, immutable rebuild and missing artifacts. No installed Provider CLI or model tokens
are used. The schema-parity tests cover every new versioned document, including the private
Notes/Head-Delta wrapper. These tests establish protocol behavior, not container containment
or measured model savings.
