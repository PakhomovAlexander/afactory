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
        "providers",
        "Provider logins: the registry, detect, setup, add, status and doctor",
        PROVIDERS,
    ),
    (
        "task-states",
        "The one Task state every af task command prints: phase, outcome, cause and next command",
        TASK_STATES,
    ),
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
    match topic {
        "config" => format!(
            "{text}\n\nWhat `.af/` holds\n\n{}\n{}\n\n{UNDECLARED}",
            review_config::layout::render_table(),
            review_config::layout::ELSEWHERE,
        ),
        // The tables are rendered from the enum every Task rendering prints, so this page and
        // the binary cannot say different things.
        "task-states" => format!("{text}\n\n{}", task_state_tables()),
        "exit-codes" => format!("{text}\n\n{}", task_exit_codes()),
        _ => text.to_string(),
    }
}

/// The phase, outcome and cause tables of `af help task-states`, from
/// `review_core::task::state`; `docs/tasks.md` carries the same rows.
pub(crate) fn task_state_tables() -> String {
    use review_core::task::state::*;
    let mut out = String::from("Phases\n\n");
    for phase in TaskStatePhaseV1::ALL {
        out.push_str(&format!(
            "  {:<10} {}\n",
            phase.as_str(),
            phase.description()
        ));
    }
    out.push_str("\nOutcomes, with the exit code af task run ends with\n\n");
    for outcome in TaskOutcomeV1::ALL {
        out.push_str(&format!(
            "  {:<18} {}  {}\n",
            outcome.as_str(),
            outcome.exit_code(),
            outcome.description()
        ));
    }
    out.push_str("\nCauses, with the outcome each belongs to\n\n");
    for cause in TaskCauseV1::ALL {
        out.push_str(&format!(
            "  {:<24} {:<18} {}\n",
            cause.as_str(),
            cause.outcome().map_or("(retained)", TaskOutcomeV1::as_str),
            cause.description()
        ));
    }
    out.trim_end().to_owned()
}

/// The Task outcome section of `af help exit-codes`.
fn task_exit_codes() -> String {
    use review_core::task::state::TaskOutcomeV1;
    let mut out = String::from(
        "`af task run`, `af task start --execute`, `af task explain` and `af review run --file` \
         end with their Task's outcome (`af help task-states`):\n\n",
    );
    let mut codes: Vec<(u8, Vec<&str>)> = Vec::new();
    for outcome in TaskOutcomeV1::ALL {
        match codes
            .iter_mut()
            .find(|(code, _)| *code == outcome.exit_code())
        {
            Some((_, names)) => names.push(outcome.as_str()),
            None => codes.push((outcome.exit_code(), vec![outcome.as_str()])),
        }
    }
    codes.sort();
    for (code, names) in codes {
        let extra = if code == 4 {
            ", or a Task that waits for a person's decision"
        } else {
            ""
        };
        out.push_str(&format!("  {code:<4} {}{extra}\n", names.join(", ")));
    }
    out.push_str(
        "\nWhere such a code's result goes to stdout alone (under --json, `af task explain`, \
         `af task run` of a Task that already finished), stderr ends with `Task ID: \
         phase/outcome/cause; next: COMMAND`, so a non-zero exit always says why there.\n\
         \n`af task show`, `af task list` and `af task report` exit 0 once they read the Store: \
         the state they print is data, not their outcome.",
    );
    out
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
changed paths, and an optional `[survey]` table bounds what the Surveyor measures into a Survey
(`af project refresh --help`). Worker packages live under `.af/workers/`, pinned in `.af/af.lock`; pipelines
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
       help page does not declare every option af passes (`setup`, `add`, `status`, `detect`)
  5    registry conflict — the ID or the auth context already belongs to another entry; under
       `detect --apply` it stops at that row, the rows before it stay registered
  6    authentication failed — the Provider CLI answered, and not with a usable login; `add`
       refuses a context that is not logged in with 6 unless --unverified
  7    usage unavailable — `provider status --usage` only: an authenticated Provider's optional
       subscription or quota probe did not answer. Authentication is unaffected

Under --json the same codes apply and the document on stdout carries the typed outcome.";

const JSON: &str = "\
stdout is the result, stderr is progress. With --json every reading command prints exactly one
JSON document on stdout — no banner, no trailing text — so `af … --json | jq` always works.
Errors under --json are one document on stdout with an `error` field and the same exit code. A
command that classifies its own outcomes — `af provider status` and `af provider setup` — prints
its own versioned document instead, carrying that exit code and an af-authored `diagnostic`;
stderr still gets one diagnostic line, so discarding stdout never hides a refusal. The Provider
documents are af/provider-detect@1 (`af provider detect`), af/provider-status@2,
af/provider-setup@1 and af/provider-models@1; none carries an account, a credential or the
Provider CLI's own words.

Documents carry `kind@version` identifiers where they describe persisted records; a payload shape
change bumps the version rather than reinterpreting an old one. Nothing af prints under --json is
affected by a TTY, colours, or the update check: its notice goes to stderr, only on a TTY, and
never under --json.

The Task documents, each named by its `schema`:

  af task show --json                  af/task-inspection@12; a collected Task:
                                       af/task-collected-inspection@2
  af task show --checks --json         af/task-checks@1
  af task list --json                  af/task-list@3, one af/task-list-entry@3 per Task
  af task report --json                af/task-report@2
  af task plan --json, refused         af/task-selection@2; an infeasible plan carries
                                       planning_diagnostic.required_limits and .reserves

Each carries the Task's `state` (`af help task-states`); a previous version is refused, never
read.";

const TASK_STATES: &str = "\
Every af task command that shows a Task prints one state for it, in text and under --json alike:
`af task list`, `af task show`, `af task report` and the last line of `af task run`. The state is
`phase/outcome/cause`, for example `finished/gate_failed/check_failed`; `outcome` is present once
the Task ended and `cause` once that outcome is not `verified`. Under --json it is the `state`
object: `phase`, `outcome`, `cause` and `next_action`, null where absent.

  planned --af task run--> running --> finished
     |                      |   ^
     |                      v   | af task run (resume)
     |                   abandoned
     +--> waiting (plan review, resources, input or a decision) --> running

A Task's wall clock starts at its first `af task run`, not when it is planned: the preview says
`deadline starts when you run`, and `af task run` prints the deadline in local time with the time
left (`deadline 17:41:18 +03 (2 h 59 min from now)`).

Every state that ended names the one command to run next, the last line of `af task show` and
`af task run` (`next: af task run ID --retry-gate`), `none` when there is none: `af task findings
ID` after reviewer findings, `af task run ID --retry-gate` after a failed check, `af task run ID`
to resume, `af task plan --file TASK_FILE --fit` after a budget or deadline ran out (TASK_FILE with
a new `task_id` once the Task ran), `af provider status` or `af provider setup ID --login` after a
Provider failure. `af task show ID --checks` prints the gate's last check receipt: each check's
failing and flaky tests and the end of its stderr.";

const PROVIDERS: &str = "\
A Provider is a login of an official model CLI, `claude` (Claude Code) or `codex` (Codex CLI),
held in one auth directory: CLAUDE_CONFIG_DIR or CODEX_HOME for that CLI. The CLI owns the
credential; af never reads one. The machine-local registry, ~/.config/af/providers.toml, records
only an ID, a kind and an auth directory per Provider, and a review or Task binds a Worker to a
Provider by that ID (--provider NODE=ID). The registry belongs to this machine, not to a
repository, so these commands run in the af you invoke, never through a project's pinned one.

In the order of first use:

  af provider detect            find the CLIs and their auth directories, and say what each
                                directory's CLI reports about its login; writes nothing
  af provider detect --apply    register every logged-in directory that is not registered yet,
                                with the IDs the table showed; never logs in
  af provider setup ID --kind KIND [--auth-dir DIR] --login
                                log in to a directory and register it; run it yourself at a
                                private terminal, never through an agent or a pipe
  af provider add ID --kind KIND --auth-dir DIR
                                register one directory that is already logged in; refused with
                                exit 6 when it is not, unless --unverified
  af provider status            the registry and one token-free login check per directory
  af provider doctor ...        the charged end-to-end check of the Providers a review binds

detect looks for each CLI on PATH, then in ~/.local/bin, ~/.claude/local, /opt/homebrew/bin,
/usr/local/bin, ~/.npm-global/bin, ~/.volta/bin and ~/.bun/bin, and for directories in the CLI
variables, ~/.claude and ~/.codex, the sibling names ~/.claude-*, ~/.config/claude,
~/.config/claude-*, ~/.codex-*, ~/.config/codex and ~/.config/codex-*, the registry, and --dir.
It reads no shell rc file, so a directory a shell alias selects needs --dir. A directory holding
none of its CLI's marker files (.claude.json, .credentials.json or settings.json; auth.json or
config.toml) is listed `empty` and not handed to the CLI, which would create files there;
--all asks anyway. status applies the same rule.

Identity: the table and every document show the plan or method (Claude Max (claude.ai),
ChatGPT, API key, not logged in), never an email or an organization. `detect --identity` adds
the account label the CLI reported, only on a terminal and never under --json.

See docs/providers.md, `af help exit-codes` and `af help json`.";

const STORAGE: &str = "\
af keeps a bounded amount of disk between runs (ADR-0144). One budget covers warm build keys,
warm Workspaces, review campaign Stores, the finished Tasks of every Task Store, Project Stores
(counted, never evicted; ADR-0158), Stores this release cannot read, and installed versions.
When af holds more than the budget, caches go first (warm keys, warm Workspaces, installed
versions), then telemetry, then records (review campaigns, unreadable Stores, Surveys, finished
Tasks), least recently used first within each. Nothing in use goes, and nothing used within the
last hour; nor the newest warm key of a project checked in the last 7 days, a finished Task
younger than keep_days or one a delivered Workspace was made from, a Store with a live writer
lease, or a Project Store another release wrote.

  af storage                  what af holds, per kind, against the budget and the free-disk floor
  af storage prune            preview the sweep: collection, then the budget
  af storage prune --apply    perform it now (it also runs after every task run and review run)

The [storage] table, machine layers only (built-in, system, user, AF_STORAGE__<KEY>); a
repository's .af/af.toml cannot set it:

  [storage]
  max_bytes = \"auto\"           the budget: 20 GiB, or 40 GiB when the cache volume has more than
                               100 GiB free (room for one warm key); or integer bytes, or a
                               number with B, KiB, MiB, GiB, TiB
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
  keep_surveys = 50            newest Surveys kept per Project; protected ones may exceed it
  keep_worker_transcripts = false   keep the history each Claude Worker Attempt and provider
                                    probe leaves in its config dir
  keep_gate_pull_requests = false   leave a finished Task's gate pull request and branches open
  sandbox_root = \"/srv/af\"     unset by default (TMPDIR): where sandboxes and check runtimes
                               are made, and so the volume the floor measures for them
  build_cache_root = \"/srv/c\"  unset by default ($XDG_CACHE_HOME/af/task-build-cache): where
                               warm build keys live, and the volume the floor measures for them

Every check gets its own empty HOME, TMPDIR, AF_CHECK_SCRATCH and XDG_CACHE_HOME, removed after
it: write scratch files there, never to /tmp. A Store made with --state is recorded in
$XDG_STATE_HOME/af/stores.toml so collection reaches it. A Task whose gate leftovers are not
cleaned up yet is never collected. The sweep that ends `af task run` is recorded on that Task:
`af task show` prints its `storage sweep:` line. A Task command prints one line for its sweeps,
`storage: freed 1.2 GiB (2 warm keys)`, and only when they did something; `--verbose` on `af task
run` and `af task start --execute` prints every line instead. When the largest warm key does not
fit max_bytes with half its size to spare, `af storage` and the run say so once, with the budget
that fits. A refusal below the floor names the volume, both numbers and what to move:
`insufficient_disk: /tmp (tmpfs): 1.9 GiB (…) free, floor 10.0 GiB (…); the sandbox root is
/tmp; set TMPDIR or [storage] sandbox_root to a volume with room`.";

const TELEMETRY: &str = "\
Telemetry records what af does while it works, in its own store, never in a Task or Campaign
Store (ADR-0153). The kernel's execution authority never reads it: identity, replay, the Ledger,
budgets, admission and every Worker input are the same whether it is on, off or deleted, and
nothing it captures reaches a prompt. Nothing leaves the machine: this release has no sink.

Levels
  off       nothing: no directory, no subscriber
  metrics   command, run, gate, check, node, Attempt, harness and sweep spans with identity and
            numeric attributes; failures; resource use; the segment header keeps argv[0] and
            the subcommand path only, and no working directory
  spans     every span and event, the configuration record, the Store commit mirror and Finding
            summaries; sizes and digests of content, never the content (the default)
  content   spans plus content blobs: Worker inputs, assistant text, reasoning, tool arguments
            and results, check output (perturbing: it keeps that content)

Credential-looking material is withheld at every level by a bounded heuristic (not a promise to
recognise every secret), before any bound cuts it: a line that
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
  af telemetry turns RUN        one row per model turn: arrival timing, the harness's per-turn
                                usage, stop reason, text and thinking sizes, tool calls
  af telemetry tools RUN        one row per tool call: name (when in the invocation's tool
                                set), arrival timing, outcome, input and output bytes
  af telemetry config RUN       a run's configuration record in one execution: version,
                                configuration with origins (an environment value is <env>),
                                plan, bindings, Providers, harness versions, host, AF_*/OTEL_*
                                names; each section complete, incomplete or unknown
  af telemetry tail [RUN]       follow the live segments: every span end and event as written
  af telemetry ledger RUN       committed Store events, Finding and Demand summaries and each
                                Attempt's charge, in order, with the coverage and history
  af telemetry context RUN ATTEMPT   at content: the Attempt's composed Worker input (the
                                prompt bytes and the manifest); --raw prints the prompt alone
  af telemetry reasoning RUN ATTEMPT at content: the Attempt's thinking blocks and reasoning
                                summaries, in order
  af telemetry search TEXT      at content: the text blobs holding TEXT, with span paths and
                                bounded excerpts; refused below content
  af telemetry gc [--apply]     what the Storage Budget's sweep evicts from the telemetry kind

Recorded: the commands that run a kernel run (af task run, af task start --execute, af review
run, af provider doctor, af self optimize --execute) record af.command > af.run >
af.authority.load, af.plan (one node record per plan node), af.node > af.attempt, and the
command's af.sweep; every other command records nothing and opens no telemetry directory. From
the first af.run until the seal one consumer thread is the segment's only writer and every
record is in it within flush_latency_ms; at the command's end it writes the command's end and
seals the segment within one bound — 2 s beyond its last progress, each content of a released
harness stream it publishes renewing it — else it is abandoned, publishes nothing, and the
segment is left for the next reader to recover. A run's executions (one per process that worked on it,
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
every run an execution works on has its own configuration record, its parts naming the run:
the configuration and machine sections when the run opens, the plan section when its plan
resolves (a run refused before that has the first two), and af.config.text naming the
pipeline's pipeline.toml, the Task file and .af/af.toml by the SHA-256 of their own bytes and
their size. A check fingerprint's runtime paths become placeholders only where a value names
the path or one under it, never a sibling (/tmp/a-extra beside /tmp/a). Every child reap of af goes
through review-process's one reap lock, so a process's af.rusage.* is never another child's.

At spans and above every event the kernel commits to a Store is mirrored after its commit, on
any path (an append, a Task transition, a Round publication, an integration), as af.store.event
with its type, sequence, id, causation, artifact references and the payload's digest and size;
a Finding summary is recorded for every Report admitted into the Ledger and whenever a
summarized field changes, with the admitting Worker's bound model when the kernel recorded it,
and a Demand summary keyed by its Demand id, with its own Subject and that Subject's Snapshot,
and its lifecycle (an admitted Evidence reuse that satisfies a stale Demand again is a
satisfaction); `af telemetry ledger` orders every Store log by its committed sequence. A mirror
failure is one problem, never a Store error, and makes the run's coverage incomplete. At
metrics and above every committed accounting transition records the Attempt's charge as the
Task projection holds it (af.usage.state, ordered by the committed event's sequence) and every
sidecar write its merged row (af.usage.wall, ordered within one execution only); an Attempt's
figures are its latest charge, run totals are sums of those, and the sidecar series never
enters them.

At spans and above every harness's streams are tapped as they arrive: the supervisor's drain
offers each chunk to a bounded queue (observer_queue_bytes) without ever waiting, a full queue
drops the chunk and counts it, and the consumer frames lines and records validated structure
only, timed by arrival: a Claude assistant message (stream-json) or a Codex turn is af.turn
with the harness's own per-turn usage, a Claude message's turn ending as soon as its line is
read (a further line of the same message is af.turn.continued on that turn, folded into its
row by turns), a Claude tool_use block or a Codex command or MCP item is af.tool with its
input and output bytes, and the other lines are events, the Claude result line by its size
only. Every identifier the harness supplies is af.corr in a live record; every untrusted
string (text, thinking, tool arguments and results, errors, unparsed lines) is only a size. A
frame too long to judge is withheld whole; a dropped chunk is af.stream.gap and the stream
incomplete. Once the process has ended Telemetry takes its own release decision:
login-challenge material in either stream or in a Codex Attempt's -o final message (read for
every observed process, whatever its ending), a final message that cannot be read whole, an
adapter that replaced the streams, or a stream the output cap truncated (a prefix where the
decision needs the whole), withholds everything pending (af.stream.withheld);
otherwise af.harness.ids lists each correlation id with its original once it passes the
credential rules (af.harness.ids.count admitted, the list going on in af.harness.ids.part
events past one record; an identifier past 4096 per process or 256 bytes has no mapping,
counted in af.harness.ids.omitted with af.harness.ids.analysis incomplete). Claude is asked
for --output-format stream-json --verbose at spans and content and for json at off and
metrics; the adapter reads the result line either way (a JSON object without a type is an
envelope only under json), and the Store keeps that line and stderr (Codex: its turn and
error lines and the -o final message), never native text around them, so accounting,
classification and the Store's bytes are the same at every level. With Claude Code 2.1.305
and claude-opus-5-5 the stream's thinking blocks carry no text, so Claude reasoning is sizes
only.

At content, and only there, Telemetry also keeps content as blobs, each redacted whole before
any truncation and named by one event: the composed Worker input of af.context.compose (the
prompt bytes, gen_ai.content.input, and the manifest, gen_ai.content.system); once the release
decision withheld nothing, a harness's assistant text and Codex agent messages
(gen_ai.content.output), its thinking blocks and reasoning items (af.content.reasoning, only
with content_reasoning) and its tool arguments and results (af.content.tool_input and
af.content.tool_output, only with content_tool_io); a check's stdout and stderr
(af.check.output); every mirrored Store event's payload; an admitted Finding's body
(af.finding.body); and .af/af.toml, the pipeline's pipeline.toml and the Task file
(af.config.text). A Worker request keeps identities only where its schema and, in a payload,
the schema of an artifact type af owns declare one; the Task's requirements and every other
payload are judged whole. An Attempt's content is bounded by content_attempt_max_bytes and
content outside any Attempt by content_process_max_bytes, counting what was stored: past
either, further content is dropped and counted (a dropped occurrence, the seal's dropped, one
budget problem) and nothing about the Worker changes. The content is the operator's: no
prompt, Warm Set, Worker Notes or optimization source ever reads it. `af telemetry context
RUN ATTEMPT [--raw]` prints an Attempt's composed input, `af telemetry reasoning RUN ATTEMPT`
its thinking and reasoning in order, and `af telemetry search 'text' [--since D] [--run RUN]`
searches the full-text index content_fts that every af telemetry command keeps at ingest when
content_index = true (and empties when it is false), reading a bounded number of matches and
showing an excerpt only of a blob it verified, a missing or corrupt one listed unreadable; below
content context and reasoning say the content was not captured and search is refused (exit 2).

The [telemetry] table, machine layers only (built-in, system, user, AF_TELEMETRY__<KEY>); a
repository's .af/af.toml cannot set it, and a value outside its bound refuses the load naming
the key:

  [telemetry]
  level = \"spans\"                      off | metrics | spans | content
  dir = \"\"                             default $XDG_STATE_HOME/af/telemetry; an absolute path
                                       that neither contains nor lies inside a Store, a cache
                                       or another telemetry directory
  max_bytes = \"4GiB\"                   256MiB to [storage] max_bytes; the storage sweep evicts
                                       sealed segments past it, status warns when over
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
  sink = \"none\"                        none: no sink exists; otlp is refused, not implemented
                                       in this release
  sink_endpoint = \"\"                   reserved: a value is refused, not implemented
  sink_headers_file = \"\"               reserved: a value is refused, not implemented
  sink_timeout_ms = 5000               100 to 60000; reserved

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
  $XDG_STATE_HOME/af/self.toml                activation history, last check, pinned projects seen
  $XDG_CACHE_HOME/af/self/latest.toml         the cached release check

Pins
  A project's `.af/af.lock` records the af release that wrote it and, per target, the digest of
  that release's archive: the pin names bytes, not just a version. When you run `af` inside such a
  project, af execs that version; an installed copy must carry the lock's digest. If it is
  absent, `[self] install_pins` decides: \"ask\" (the default) asks on the terminal, naming the
  version, the lock and whether it is older than this af or a pre-release, and without a
  terminal installs nothing; \"always\" installs; \"never\" does not (true and false mean always
  and never). Whatever is not installed leaves this af to run an older pin's command, printing
  `af self install <v>`; a newer pin is refused. `af review plan` never installs. An install
  from a lock needs the archive to match both the lock's digest for this machine's target and
  the release's signed SHA256SUMS. A pin without a digest for this target is never installed
  on demand: `af self install <v>` trusts the release checksums explicitly, and
  `af onboard --refresh-lock` run online records every target. AF_SELF_OFFLINE=1 refuses too.
  AF_VERSION=<v> overrides the pin for one invocation; `af onboard --refresh-lock --af <v>`
  moves the pin to <v> (that release writes the lock).

  Only a released, receipted binary writes a pin; a source build leaves the lock unpinned or the
  existing pin untouched. Nothing older than 0.8.0 — the oldest supported release — is ever
  dispatched to or made the default.

What binds bytes
  The release's SHA256SUMS, which every release signs (minisign) with the key embedded in the
  binary at build time; an unsigned or badly signed SHA256SUMS is refused. Under a lock, the
  lock's digest as well: both must match. `af self status` shows whether this build carries the
  key; a receipt's verified_by says which checks installed each version (lock+signature,
  minisign, or sha256sums when a build without a key, or install.sh under
  AF_INSTALL_ALLOW_UNSIGNED=1, checked the checksum alone).

Updates — `[self]` in ~/.config/af/config.toml
  update_check = true      look for a newer release, at most once per check_every
  check_every = \"24h\"
  auto_update = \"notify\"   notify | always | never
  channel = \"stable\"       stable | rc
  install_pins = \"ask\"     ask | always | never
  keep_versions = 3

  The check never runs on the hot path. After an eligible command — any but help, --version,
  `af self`, `af config`, completions and the browser — af spawns a detached child that
  refreshes the cache and exits, with or without a terminal and under --json too; `af self
  status` schedules it too when it is due, and `af self status --refresh` runs it at once. The
  child holds none of the command's pipes, and never changes its output or exit status.
  check_every throttles invocations, counted from the last attempt, a failed one included: no
  timer, daemon or service runs between them. Releases come anonymously over HTTPS (curl) from
  a public repository, through gh only when that is refused; every call is bounded (curl: 5 s to
  connect, 30 s to list releases; gh: 30 s), and a failure's reason (the HTTP status or curl's
  error) is said before any `gh auth login` hint. The cache (af/self-check@3) records the last
  answer, the last attempt and why it failed, which `af self status` prints:
  `last check: <time> · latest X · last attempt <time> failed: <reason>`. A check that has held
  latest.lock for over an hour counts as no check, and status names it with `rm <lock>`. A
  pre-release af asks for pre-releases whatever the channel, and is told `latest stable X ·
  latest pre-release Y (you run R: af self update --rc)`; a release older than the running one
  is never shown as simply `latest`. CI, AF_SELF_OFFLINE, update_check = false and auto_update =
  \"never\" turn the check off; `af self update --check` offline prints `offline: check skipped`
  and exits 0. Inside a pinned project the af that dispatches schedules it, once. Only an interactive
  run — stderr a terminal, no --json — prints the cached notice, one line on stderr; under a
  pin, the af that dispatches prints it. With `always`, the child installs the newer release
  and retargets the default, unless `af self update` or `rollback` changed it meanwhile; the
  running process and every pinned project are untouched. Its \"updated the default\" notice is
  shown only while the activation it announces stands: one a later update or rollback made
  stale, even an update back to the same version, is dropped. `af self update` without
  --version never moves the default to a release older than the running one.

Commands
  af self status [--refresh] | update [--check] | rollback | install V | remove V | prune
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
