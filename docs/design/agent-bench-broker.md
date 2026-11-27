# Restricted source-navigation broker

This design describes the three common tools of `af_sandbox::navigation::Broker`.
The [installed paired benchmark](../agent-bench.md) also uses its restricted semantic extension.
It is partial overall agent-bench implementation. It does not collect native Provider usage,
start benchmark Tasks, grade answers, or claim performance improvement. All committed test data
is synthetic. The [offline analyzer](agent-bench-offline.md) and its repairs remain unchanged.

## Callable host API

A trusted host selects exact source files, excluding oracle, evaluator and history data, and calls
`Broker::new(BTreeMap<String, Vec<u8>>)`. This accepts file bytes only, never a checkout directory,
symlink, FIFO or device. Source, including binary entries, is limited to 128 MiB across at most 16,384 files, each at most 8 MiB. Paths use a deliberately
narrow ASCII alphabet, at most 240 bytes, with no empty, dot, parent, or VCS history components (case insensitive). Inconsistent component
spellings such as `Src/a` beside `src/b` are refused on every host to prevent staging aliases.
`Broker::from_manifest(&Manifest, &Cas)` admits the entire exact native Manifest, checks every
entry's content and size, and preserves executable modes in its content identity. Links and
unsupported paths are refused rather than omitted. Staged files are all non-executable.
The API creates private host-owned immutable staging; it exposes no staging path or writable handle.
An owner-only (0700) envelope protects the staged tree even with a shared `TMPDIR=/tmp`, including
when cleanup is unconfirmed. Only its read-only child is mounted into Docker, allowing the mapped
nonroot identity to read it without granting unrelated host users access through the envelope.
The host remains trusted to select the correct source; a file named like an ordinary source file
cannot be identified as an oracle from its contents.

`Request::parse(&[u8])` admits at most 4096 bytes of JSON. Duplicate keys, unknown fields/tools,
malformed requests and `request_user_input` fail locally without owner interaction.
`Broker::execute(&mut self, &Request) -> io::Result<Response>` provides exactly:

- `{"tool":"af_list_paths","offset":0}`: up to 256 sorted source file paths. Start at zero,
  then use the returned `next_offset` until it is null. Offsets beyond EOF are refused.
- `{"tool":"af_read_range","path":"src/lib.rs","offset":0,"length":64}`:
  a UTF-8 byte range; length is 1–65536, offset is a nonnegative u32. An offset beyond EOF or
  a range splitting a UTF-8 character is refused; a range reaching EOF returns the available bytes.
  A non-UTF-8 file is explicitly returned as `text: null`.
- `{"tool":"af_search","query":"needle","offset":0}`: literal ASCII substring search, at most 256 bytes,
  without NUL/CR/LF, returning sorted paths and one-based line numbers with complete matching lines. Only LF delimits lines; CR and all other
  characters remain in the line, including admitted vertical-tab and form-feed queries.
  Each page contains at most 128 hits and an explicit nullable `next_offset`; use that cursor
  to continue. `non_text_paths` identifies binary files skipped while scanning that page.
  LF iteration uses bounded memory even for an 8 MiB file of short nonmatching lines.

Responses carry the same `tool` discriminator and explicit completeness fields described above.
The [wire schema](../../schemas/source-navigation-v3.json) describes requests and responses;
byte bounds and source relationships also require executable admission. Prior versions are retired
without fallback under ADR-0145. The Rust request/response types and tool parameter projection
live in `af_contracts::navigation` and are shared directly with the restricted transport.
Every response is limited to 256 KiB encoded JSON; an oversized matching line is refused. There is no truncation
that could be mistaken for complete results. No command, shell, build, test, write, URL, network
or owner-input operation exists in this API. Even a file containing executable instructions is
only source bytes. Listings and search also reject symlinks and special files.

`source_identity()` is the native Manifest content digest, covering every path, content digest,
size and executable mode. `new` treats its byte-map entries as ordinary files; use `from_manifest`
for captured executable-mode provenance.
`conformance_identity()` hashes the JSON tuple of protocol identity `af.source-navigation/3`,
pinned image, exact worker program, host module, shared admission module and schema bytes. `request_identity()` and `response_identity()`
hash typed Rust JSON serialization. Hashes are SHA-256 with `sha256:` prefixes. These are
reproducible identities, not a claim that conformance ran or native Task receipts exist. Future
transport must retain them with its own native receipts; this component adds no ledger or scheduler.

## Docker lifecycle and containment

Only Docker is implemented. Unavailable Docker, a missing pinned local image, unsupported host
permissions, malformed output, nonzero exit, held pipes or truncation fail closed. There is no
local-process fallback or automatic image pull. The image is pinned to:

```text
python@sha256:aa943ce411423c7582288f18707ff66eddcbaeb510502d747a72aeb34f30d159
```

The container has network `none`, user `65534:65534`, a read-only root filesystem, all capabilities
dropped, `no-new-privileges`, 32 PIDs, 128 MiB memory and one CPU. The sole host mount is immutable
staging at `/source`, read-only. No checkout, oracle, sibling, history, Docker socket, HOME or
credential mount is supplied. Docker client environment is cleared, PATH sanitized, and its config
is a fresh empty directory; host authentication is not loaded. Docker uses its default local socket.
The fixed worker runs Python isolated mode with bytecode writes disabled. Caller data arrives only
on stdin and is never interpreted as code. Symlinks are refused at every component; regular-file
opens use `O_NOFOLLOW | O_NONBLOCK`. Immutable staging prevents request-time replacement races.

Every Docker command uses `af-process` supervision with a 30-second deadline and bounded exit/drain.
Each request creates a uniquely named container, starts it, then force-removes its exact returned container ID even after a start failure.
Removal ends the container PID namespace, including descendants which could outlive the Docker
client. Cleanup must succeed before returning source data. A failed or ambiguous create never grants removal authority over an existing named container.
Unconfirmed creation or cleanup preserves staging,
disables that Broker, and reports its path for operator recovery.
`execute_until(request, absolute_deadline)` shares the subject deadline across create and start,
with at most 30 seconds per helper. Expiry refuses new creation/start before invoking a helper.
Only exact-container cleanup has a separate bounded grace after expiry; no source data is
returned after the subject deadline. `execute` supplies a 30-second request deadline.

## Credential-free conformance

With Docker running and the pinned image already installed, from the repository root run exactly:

```sh
cargo nextest run --workspace --locked --profile ci --run-ignored all \
  -E 'binary_id(af-sandbox) & test(/^navigation::tests::/)'
```

The explicitly ignored Docker tests are included by this command and fail if Docker/image is
unavailable. A 4,097-file fixture checks complete paginated navigation and final-file read/search;
a control-character fixture checks literal matching and LF line numbering. They also exercise actual production broker dispatch for list/read/search, missing external
files, escaping links, FIFOs and complete search pagination. Mixed text/binary fixtures and a
near-limit short-line fixture exercise the production memory and source-identity boundaries. A separate synthetic substrate probe reuses
production create flags to check nonroot/capabilities/no-new-privileges, source/root write denial,
no external marker/socket and failed external network connection. The deadline test starts
a fixed parent/child process, times out the Docker client and checks confirmed container removal
and subsequent broker reuse. A name-collision test checks that failed creation preserves staging
and disables the broker without deleting the other container. This probe deliberately runs
fixed test-only code; no such execution port is exposed to callers. The ordinary admission and
schema tests alone are not proof of Docker containment. No credentials or model calls are used.

The installed credential-free smoke also connects the restricted Codex transport and its raw-call
hooks to the Task Store. Its treatment calls mount only the pinned Linux analyzer and runtime
libraries alongside the immutable source. Full external schedule admission and independent
grading remain outstanding, as do authenticated collection, A/A and conditional exploratory A/B.
The historical accepted design artifact is
`sha256:8bf9fbce95f3671ea79108b06ae557c4cfacf3ccbd9771333da87aec8bdf3353`;
its source and recommendation remain recorded in the offline analyzer note. This broker implements
only its source-only Docker boundary and narrow host API. It grants no live execution or publication
authority, and does not assert full benchmark acceptance.
