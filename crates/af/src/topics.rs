//! Long-form help pages: `af help <topic>`, also rendered as `man af-<topic>`.

pub(crate) const TOPICS: &[(&str, &str, &str)] = &[
    ("config", "How af is configured", CONFIG),
    (
        "layers",
        "The configuration ladder and where each file lives",
        LAYERS,
    ),
    ("environment", "Environment variables af reads", ENVIRONMENT),
    ("exit-codes", "What every exit code means", EXIT_CODES),
    ("json", "The --json contract", JSON),
    (
        "self",
        "Installing, updating, pinning, and removing af",
        SELF_TOPIC,
    ),
    (
        "storage",
        "The disk af keeps between runs, and the [storage] table",
        STORAGE,
    ),
    (
        "telemetry",
        "What af records about its own work, the four levels, and the [telemetry] table",
        TELEMETRY,
    ),
];

pub(crate) fn find(name: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    TOPICS.iter().find(|(topic, _, _)| *topic == name)
}

/// One topic's body. The configuration page ends with the declared `.af/` layout, rendered from
/// `review_config::layout` rather than stored a second time here: that table, the one in
/// the documentation and every classification are the same data.
pub(crate) fn body(topic: &str, text: &str) -> String {
    if topic != "config" {
        return text.to_string();
    }
    format!(
        "{text}\n\nWhat `.af/` holds\n\n{}\n{}\n\n{UNDECLARED}",
        review_config::layout::render_table(),
        review_config::layout::ELSEWHERE,
    )
}

/// What the same table says about a captured Snapshot, and the one knob over it.
const UNDECLARED: &str = "\
`af task plan` and `af task start` classify the Snapshot they captured against that table and
warn about any path under `.af/` no entry names, with the count, the byte total and up to ten
paths; `--json` carries the whole list in `undeclared_af_paths`, and `af task deliver` records it
in the receipt. Nothing is removed: not from the Snapshot, not from the delivered worktree. Set
`[delivery] undeclared_af_paths = \"refuse\"` in `.af/af.toml` to make delivery stop instead,
before it prepares a record or touches git. The value is captured when a Task is planned.";

const CONFIG: &str = "\
Everything a human writes is TOML; everything af writes goes to the Store, except `.af/af.lock`
(machine-written TOML, committed, like Cargo.lock).

The project file is `.af/af.toml` at the git toplevel. `version`, `[project]` `name` and
`min_af`, and `[defaults]` `pipeline` are required: they name the project, the minimum `af`
release it needs and its default review pipeline. Optional `[[routes]]` pick a pipeline by the
changed paths. Worker packages live under `.af/workers/`, pinned in `.af/af.lock`; pipelines
name them.

  version = 1
  [project]   name = \"myrepo\"      min_af = \"0.7\"
  [defaults]  pipeline = \"review\"

The user file is `~/.config/af/config.toml` (XDG on macOS too). It holds what belongs to the
person and the machine, never to the project: the `[self]` update policy, the `[storage]`
budget (see `af help storage`) and the `[telemetry]` level (see `af help telemetry`). Provider
logins and
cache policy stay in `providers.toml` and `caches.toml` beside it.

`af config show` prints the effective configuration; `--origin` names the file and line each
value came from; `af config paths` lists every file a layer would read. See `af help layers`.";

const LAYERS: &str = "\
Lowest to highest. Tables deep-merge, scalars last-wins, arrays replace.

  built-in    the binary's defaults — everything has one
  system      /etc/af/config.toml and /etc/af/conf.d/*.toml
  user        $XDG_CONFIG_HOME/af/config.toml and conf.d/*.toml (~/.config/af)
  directory   .af/af.toml in every directory ABOVE the repository's git toplevel, from / down;
              nearer wins. Outside a repository: every ancestor of the current directory.
  project     .af/af.toml at the git toplevel (found by walking up; nothing below it is read)
  local       .af/af.local.toml beside it, gitignored — this checkout's overrides
  environment AF_<TABLE>__<KEY>, e.g. AF_SELF__AUTO_UPDATE=never

A directory layer's `.af/af.toml` merges into `af config show` for every checkout beneath it.
`[self]`, `[storage]` and `[telemetry]` are machine-only: a directory, project or local layer
that carries one is reported and ignored.

Branch-specific configuration needs no layer: `.af/` is versioned, so the project layer on a
branch is whatever that branch commits. Each git worktree has its own `.af/` copy and its own
`af.local.toml`.";

const ENVIRONMENT: &str = "\
  AF_<TABLE>__<KEY>       override one configuration value (highest layer), e.g.
                          AF_TELEMETRY__LEVEL=content for one run
  AF_VERSION              run this installed version regardless of the project's pin
  AF_SELF_OFFLINE=1       never contact a release source: no pin installs, no update checks
  AF_RELEASE_SOURCE       a local directory of releases (<tag>/<asset>) instead of GitHub
  AF_RELEASE_KEY          a minisign .pub file to verify SHA256SUMS with, instead of the embedded key
  AF_PROVIDERS_FILE       absolute path of the provider registry (default: ~/.config/af/providers.toml)
  AF_CACHE_POLICY_FILE    absolute path of machine-local cache policy (default: ~/.config/af/caches.toml)
  AF_DISPATCHED_FROM      set by af when it execs a pinned version; never set it yourself
  XDG_CONFIG_HOME         config          (~/.config)
  XDG_STATE_HOME          state           (~/.local/state)
  XDG_DATA_HOME           installed versions, receipts, man pages (~/.local/share)
  XDG_CACHE_HOME          caches, the release check (~/.cache)
  XDG_BIN_HOME            where the default `af` symlink lives (~/.local/bin)
  NO_COLOR, CLICOLOR_FORCE, EDITOR, CI    honoured as their conventions say
  HOME                    required; must be absolute";

const EXIT_CODES: &str = "\
  0    pass — the command succeeded; for `review run`, the Round passed
  1    error — af could not do what was asked; stderr names the entity, the knob, and the fix
  2    usage — the command line was wrong; only the failing command's usage is printed
  3    fail — the review or evaluation ran to completion and did not pass
  4    incomplete — the run ended without a verdict (budget, timeout, missing coverage)
  10   outdated — `af self update --check`: a newer release exists

`af provider` classifies its own refusals, so 3 and above name a Provider condition there:

  3    human action required — a login is needed; af prints the exact command a human runs at a
       private interactive terminal and never starts the OAuth exchange itself
  4    Provider CLI missing, cannot start or incompatible — the official `claude` or `codex`
       binary is not on PATH, is not executable, exits non-zero on its own version check, or its
       help page does not declare every option af passes (`setup`, `status`)
  5    registry conflict — the ID or the auth context already belongs to another entry
  6    authentication failed — the Provider CLI answered, and not with a usable login
  7    usage unavailable — `provider status --usage` only: an authenticated Provider's optional
       subscription or quota probe did not answer. Authentication is unaffected

Under --json the same codes apply and the document on stdout carries the typed outcome.";

const JSON: &str = "\
stdout is the result, stderr is progress. With --json every reading command prints exactly one
JSON document on stdout — no banner, no trailing text — so `af … --json | jq` always works.
Errors under --json are one document on stdout with an `error` field and the same exit code. A
command that classifies its own outcomes — `af provider status` and `af provider setup` — prints
its own versioned document instead, carrying that exit code and an af-authored `diagnostic`;
stderr still gets one diagnostic line, so discarding stdout never hides a refusal.

Documents carry `kind@version` identifiers where they describe persisted records; a payload shape
change bumps the version rather than reinterpreting an old one. Nothing af prints under --json is
affected by a TTY, colours, or the update check: its notice goes to stderr, only on a TTY, and
never under --json.";

const STORAGE: &str = "\
af keeps a bounded amount of disk between runs (ADR-0144). One budget covers warm build keys,
warm Workspaces, review campaign Stores, the finished Tasks of every Task Store, Stores this
release cannot read, and installed versions. When af holds more than the budget, the least
recently used entry goes first; nothing in use and nothing used within the last hour ever goes.

  af storage                  what af holds, per kind, against the budget and the free-disk floor
  af storage prune            preview the sweep: collection, then the budget
  af storage prune --apply    perform it now (it also runs after every task run and review run)

The [storage] table, machine layers only (built-in, system, user, AF_STORAGE__<KEY>); a
repository's .af/af.toml cannot set it:

  [storage]
  max_bytes = \"20GiB\"          the budget; integer bytes or a number with B, KiB, MiB, GiB, TiB
  min_free_bytes = \"10GiB\"     below this free space on any volume af works on, or when it
                               cannot be measured, checks, measurement repetitions and Worker
                               Attempts are refused with insufficient_disk before any token is
                               spent
  auto_gc = false              also collect by age (keep_days, keep_tasks, keep_campaigns) after
                               every `af task run` and `af review run`; off, a run only holds af
                               to max_bytes, and `af storage prune --apply` collects on request
  keep_days = 14               collection never takes anything used in the last keep_days
  keep_tasks = 20              newest finished Tasks kept per Store
  keep_campaigns = 20          newest review campaigns kept
  keep_worker_transcripts = false   keep the history each Claude Worker Attempt and provider
                                    probe leaves in its config dir
  keep_gate_pull_requests = false   leave a finished Task's gate pull request and branches open

Every check gets its own empty HOME, TMPDIR, AF_CHECK_SCRATCH and XDG_CACHE_HOME, removed after
it: write scratch files there, never to /tmp. A Store made with --state is recorded in
$XDG_STATE_HOME/af/stores.toml so collection reaches it. A Task whose gate leftovers are not
cleaned up yet is never collected. The sweep that ends `af task run` is recorded on that Task:
`af task show` prints its `storage sweep:` line.";

const TELEMETRY: &str = "\
Telemetry records what af does while it works, in its own store, never in a Task or Campaign
Store (ADR-0153). The kernel's execution authority never reads it: identity, replay, the Ledger,
budgets, admission and every Worker input are the same whether it is on, off or deleted, and
nothing it captures reaches a prompt. Nothing leaves the machine unless `sink = \"otlp\"` is set.

Levels
  off       nothing: no directory, no subscriber
  metrics   command, run, gate, check, node, Attempt, harness and sweep spans with identity and
            numeric attributes; failures; resource use
  spans     every span and event, the configuration record, the Store commit mirror and Finding
            summaries; sizes and digests of content, never the content (the default)
  content   spans plus content blobs: Worker inputs, assistant text, reasoning, tool arguments
            and results, check output (perturbing: not bounded by the overhead gate)

Credential-looking material is withheld at every level, before any bound cuts it: a line that
looks like a credential becomes `[line withheld: looks like credential material]`, a JSON member
named like a secret keeps its key and loses its value (`[withheld]`), a configuration row whose
key is named like a secret or itself looks like a credential loses its value and origin (and
such a key is withheld too), a line or document too long to judge whole is withheld whole, an
attribute whose name itself looks like a credential is omitted and counted in `af.redacted`,
and an attribute or a command-line option named like a secret keeps its name and loses its
value whatever its shape. Only a kernel identity (a digest, a 16, 32 or 64 hex id, a segment or
occurrence id, an ordinal) in an identity position (af.run_id, span_id, a digest field, ...)
escapes these rules; any other value is judged as text. Token and charge counters are exact
decimal strings: any other number under their names is dropped and counted. A HOME that gives
no home prefix to replace by `~` is recorded as a problem. A telemetry-only failure (an
unwritable directory, a full segment) is one problem and the run continues.

  af telemetry status           level, directory, bytes, segments, runs, index, problems
  af telemetry reindex          recover crashed writers' segments and rebuild the index
  af telemetry runs             every run: executions, active, execution and elapsed time,
                                outcome, charge, failures, coverage, retained history
  af telemetry show RUN         the span tree: start offset, total and self time, status
  af telemetry config RUN       an execution's configuration record: version, configuration
                                with origins (an environment value is <env>), plan, bindings,
                                Providers, harness versions, host, AF_*/OTEL_* names
  af telemetry tail [RUN]       follow the live segments: every span end and event as written
  af telemetry ledger RUN       committed Store events, Finding and Demand summaries and each
                                Attempt's charge, in order, with the coverage and history
  af telemetry gc [--apply]     what the Storage Budget's sweep evicts from the telemetry kind

Recorded: the commands that run a kernel run (af task run, af task start --execute, af review
run, af provider doctor, af self optimize --execute) record af.command > af.run >
af.authority.load, af.plan (one node record per plan node), af.node > af.attempt, and the
command's af.sweep; every other command records nothing and opens no telemetry directory. From
the first af.run until the seal one consumer thread is the segment's only writer and every
record is in it within flush_latency_ms; at the command's end it writes the command's end and
seals the segment within one 2 s bound, else it is abandoned, publishes nothing, and the segment
is left for the next reader to recover. A run's executions (one per process that worked on it,
numbered by the execution markers, TaskExecutionStarted@1 events of the Task's log, the Store
holds for the leases before it) can
overlap: active time is their union, execution time their sum, elapsed time first start to last
end. Its history is
`prefix:k` or `partial`, never complete, and its open executions are named beside the sealed
ones (retained executions 1–1, open: 2).

Below the run every phase is a span of the crate that owns it (the catalogue is in
docs/architecture.md): af.admission, af.context.compose, af.harness, af.output.parse and
af.usage.account under an Attempt; af.gate and its af.check spans, af.warm.restore and
af.warm.save under a check node; af.snapshot.capture, af.cas.materialize, af.store.append,
af.store.read, af.git, af.remote_check and af.process wherever the run does them. Every process
span carries process.pid, process.command (the program only), its exit, its stream sizes and
af.rusage.user_ms, af.rusage.system_ms and af.rusage.max_rss_bytes, each absent when the
platform did not deliver it. An af.check carries af.check.status and af.check.fingerprint, the
comparability key af/check-fingerprint@1 (or `unknown`). A kernel measurement is a run of its
own (af.run.kind = measure, af.measurement.id), its repetitions child spans. At spans and above
each execution's run starts with its configuration record.

At spans and above every event the kernel commits to a Store is mirrored after its commit, on
any path (an append, a Task transition, a Round publication, an integration), as af.store.event
with its type, sequence, id, causation, artifact references and the payload's digest and size;
a Finding summary is recorded when a Report is admitted into the Ledger and whenever its
disposition changes, and a Demand summary keyed by its Demand id with its lifecycle. A mirror
failure is one problem, never a Store error, and makes the run's coverage incomplete. At
metrics and above every committed accounting transition records the Attempt's charge as the
Task projection holds it (af.usage.state, ordered by the committed event's sequence) and every
sidecar write its merged row (af.usage.wall, ordered within one execution only); an Attempt's
figures are its latest charge, run totals are sums of those, and the sidecar series never
enters them.

The [telemetry] table, machine layers only (built-in, system, user, AF_TELEMETRY__<KEY>); a
repository's .af/af.toml cannot set it, and a value outside its bound refuses the load naming
the key:

  [telemetry]
  level = \"spans\"                      off | metrics | spans | content
  dir = \"\"                             default $XDG_STATE_HOME/af/telemetry; an absolute path
                                       that neither contains nor lies inside a Store, a cache
                                       or another telemetry directory
  max_bytes = \"4GiB\"                   256MiB to [storage] max_bytes; counted in the budget
  flush_bytes = \"64KiB\"                4KiB to 16MiB: the buffer written in one write
  flush_latency_ms = 1000              50 to 10000: the longest a record waits while a run is open
  observer_queue_bytes = \"4MiB\"        256KiB to 64MiB: the stream queue; full, it drops chunks
  segment_max_bytes = \"256MiB\"         16MiB to 4GiB: one process's segment, every record kind,
                                       the last 64 KiB kept for its seal
  content_max_blob_bytes = \"16MiB\"     64KiB to 256MiB: larger content keeps head and tail
  content_attempt_max_bytes = \"512MiB\" 16MiB to 8GiB: content one Attempt may keep
  content_process_max_bytes = \"64MiB\"  4MiB to 1GiB: content outside any Attempt
  content_native_body_max_chars = 1048576   4096 to 8388608
  content_reasoning = true             keep thinking blocks and reasoning summaries at content
  content_tool_io = true               keep tool arguments and results at content
  content_model_bodies = true          keep raw API bodies at content
  content_index = true                 maintain the full-text index over text blobs
  harness_native = false               receive the harness's own telemetry on loopback
  sink = \"none\"                        none | otlp: push after each run
  sink_endpoint = \"\"                   an http:// or https:// URL; required with otlp only
  sink_headers_file = \"\"               one `Name: value` per line, read at push time
  sink_timeout_ms = 5000               100 to 60000

Layout
  $XDG_STATE_HOME/af/telemetry/v1/segments/   one JSONL segment per process: live under its
                                              writer's lock, sealed and compressed at exit
  $XDG_STATE_HOME/af/telemetry/v1/blobs/      content-addressed, compressed, shared
  $XDG_STATE_HOME/af/telemetry/v1/index.sqlite   rebuildable; `af telemetry reindex`
  $XDG_STATE_HOME/af/telemetry-dirs.v1.json   every telemetry directory ever written here

`af telemetry status` accounts every byte beneath the telemetry directory: a file or directory
that matches none of the above, or a symbolic link anywhere beneath it, is counted once under an
`unrecognized` entry (its count, its allocated bytes and up to 16 of its names), never read,
followed or removed. It rechecks each registered directory's overlap: none, overlapping, or
unknown when the Store registry cannot be read (named among the problems; the exit code is
unchanged).

Every registered telemetry directory is the kind `telemetry` in `af storage`, whatever the
current level or dir: one entry per segment, whose bytes are what its eviction frees, and the
directory's shared remainder (index, locks, shared blobs, temporaries). A sealed segment's last
use is its seal time. The sweep evicts sealed and corrupt segments least recently used first
past [telemetry] max_bytes or the whole budget, never one in use or used within the hour, each
whole or not at all — any of its files (its manifest included) changed since the inventory
leaves it in place, reported as changed — and reclaims temporary files, unreferenced blobs and
free index pages. A directory overlapping other accounted state is `overlapping`: counted once,
never evicted as telemetry.

There is no telemetry keep_days: collection by age is [storage] keep_days under [storage]
auto_gc or an applied prune (af storage prune --apply, af telemetry gc --apply). See
docs/telemetry.md.";

const SELF_TOPIC: &str = "\
Layout
  $XDG_DATA_HOME/af/versions/<v>/af          every installed version, immutable once verified
  $XDG_DATA_HOME/af/versions/<v>/receipt.toml what was installed, from where, verified how
  $XDG_BIN_HOME/af  ->  versions/<v>/af       the default; the symlink is the record
  $XDG_STATE_HOME/af/self.toml                activation history, last check
  $XDG_CACHE_HOME/af/self/latest.toml         the cached release check

Pins
  A project's `.af/af.lock` records the af release that wrote it and, per target, the digest of
  that release's archive: the pin names bytes, not just a version. When you run `af` inside such a
  project, af execs that version; if it is absent, af installs it only when the archive it
  downloads matches the lock's digest for this machine's target, and an installed copy must carry
  that digest too. A pin without a digest for this target is never installed on demand:
  `af self install <v>` trusts the release checksums explicitly, and `af onboard --refresh-lock`
  run online records every target. AF_SELF_OFFLINE=1 or `[self] install_pins = false` refuses,
  printing the command to run. AF_VERSION=<v> overrides the pin for one invocation;
  `af onboard --refresh-lock --af <v>` moves the pin to <v> (that release writes the lock).

  Only a released, receipted binary writes a pin; a source build leaves the lock unpinned or the
  existing pin untouched. Nothing older than 0.8.0 — the oldest supported release — is ever
  dispatched to or made the default.

What binds bytes
  Under a lock: the lock's digest. Outside one: the release's SHA256SUMS, which every release
  signs (minisign) with the key embedded in the binary at build time; an unsigned or badly
  signed SHA256SUMS is refused. `af self status` shows whether this build carries the key; a
  receipt's verified_by says which check installed each version (lock, minisign, or sha256sums
  when a build without a key or install.sh without minisign checked the checksum alone).

Updates — `[self]` in ~/.config/af/config.toml
  update_check = true      look for a newer release, at most once per check_every
  check_every = \"24h\"
  auto_update = \"notify\"   notify | always | never
  channel = \"stable\"       stable | rc
  install_pins = true
  keep_versions = 3

  The check never runs on the hot path. After an eligible command — any but help, --version,
  `af self`, `af config`, completions and the browser — af spawns a detached child that
  refreshes the cache and exits, with or without a terminal and under --json too. The child holds
  none of the command's pipes, and never changes its output or exit status. check_every
  throttles invocations, a failed attempt included: no timer, daemon or service runs between
  them. CI, AF_SELF_OFFLINE, update_check = false and auto_update = \"never\" turn the check
  off. Inside a pinned project the af that dispatches schedules it, once. Only an interactive
  run — stderr a terminal, no --json — prints the cached notice, one line on stderr; under a
  pin, the af that dispatches prints it. With `always`, the child installs the newer release
  and retargets the default, unless `af self update` or `rollback` changed it meanwhile; the
  running process and every pinned project are untouched. Its \"updated the default\" notice is
  shown only while the activation it announces stands: one a later update or rollback made
  stale, even an update back to the same version, is dropped.

Commands
  af self status | update [--check] | rollback | install V | remove V | prune
  af self setup-shell [--write] | uninstall [--purge]
  Update, rollback, remove, prune, and uninstall refuse to touch a binary af did not install.
  uninstall refuses while a background check runs, since it may be installing a release: run
  it again when the check has finished. A check that starts after an uninstall writes nothing,
  and a command that was running during an uninstall --purge recreates none of what it removed.
  From a cargo/source-installed af at the default path, `af self install V` explicitly replaces
  that running executable with the verified release and adopts the self-managed layout. An
  unrelated file at the default path is never replaced.
  remove and prune keep the default and every version a project seen on this machine pins,
  read again under the lock every activation holds. A pinned release runs only once its pin is
  recorded under that lock: when it cannot be, the command fails and says why.";
