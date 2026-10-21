//! The `af` command tree: one clap definition feeds parsing, `--help`, `af help <topic>`, shell
//! completions, and the man pages, so none of them can drift from the others.
//!
//! Help follows three rules. `af` alone shows the namespaces (on a terminal it opens the browser
//! instead, see `tui`); `af <namespace>` shows only that namespace; `af <command> --help` says
//! what the command does, what it never does, its options grouped by role, and examples.

use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use clap_complete::engine::ArgValueCompleter;

use crate::selfmgmt::{complete_campaign, complete_version};

pub(crate) const REVIEW_PIPELINE: &str = ".af/pipelines/review.toml";

const AF_ABOUT: &str = "Afactory — deterministic multi-agent review and implementation";
const AF_LONG_ABOUT: &str = "\
af drives sandboxed, budgeted Worker attempts over an immutable Snapshot of a git repository \
and folds their typed results into a findings ledger. Project policy lives in `.af/` (committed \
TOML); state lives outside the repository under XDG directories; every model call is bounded \
and journaled.

Namespaces:
  review     plan, run, and work a review Campaign
  task       start, inspect, and deliver an implement Task
  project    show this repository's Project and manage its Workspaces
  catalog    capture shared Pipeline, Worker and Task-kind packages from Git
  provider   inspect and preflight the configured model providers
  onboard    generate or validate `.af/` review authority for a repository
  config     show the effective configuration and where each value came from
  self       optimize the current project, or manage af installations

`af help <topic>` explains config, layers, environment, exit-codes, json, and self. `af` alone \
at a terminal opens a read-first browser over this scope's settings, providers and pipelines.";

const AF_AFTER_HELP: &str = "\
Exit codes:
  0 pass · 1 error · 2 usage · 3 fail · 4 incomplete · 10 outdated (self update --check)

Examples:
  af onboard --apply             generate `.af/` for this repository (token-free preview first)
  af review plan --policy-rev HEAD --base HEAD --uncommitted
  af review --policy-rev HEAD --base HEAD --uncommitted
  af self status                 what is installed and which pin applies here

Review and commit generated `.af/` before selecting it with --policy-rev. These review examples
use trusted policy at HEAD and compare working-tree changes against HEAD; run refuses an empty diff.
To execute, also supply --provider NODE=ID for every model Worker required by that policy.";

#[derive(Debug, Parser)]
#[command(
    name = "af",
    bin_name = "af",
    about = AF_ABOUT,
    long_about = AF_LONG_ABOUT,
    after_long_help = AF_AFTER_HELP,
    disable_help_subcommand = true,
    disable_version_flag = true,
    max_term_width = 100
)]
pub(crate) struct Af {
    /// Print the version (add --json for commit, target, and receipt)
    #[arg(long, short = 'V', global = false)]
    pub(crate) version: bool,
    /// With --version: one JSON document instead of a line
    #[arg(long, requires = "version")]
    pub(crate) json: bool,
    /// With no command on a terminal: the repository the browser opens [default: .]
    #[arg(long, value_name = "DIR")]
    pub(crate) repo: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Command {
    /// Capture shared Pipeline, Worker and Task-kind packages from an exact Git revision
    #[command(
        long_about = "Capture shared Pipeline, Worker and Task-kind packages from an exact Git revision.\n\n\
`init` writes starter packages and `sync` writes a captured import into --destination. The \
destination is a safe project-relative path under --repo (default `.`) that must not exist yet: \
an absolute path, a backslash, an empty, `.`, `..` or `.git` segment, or a path through a \
symlink or a non-directory is refused, and an existing destination is never overwritten. \
`init --profile` names a starter set, not an installed Task-kind profile.",
        after_long_help = "Examples:\n  af catalog init --profile software --destination task-demo\n  af catalog sync --source ../team-pipelines --revision main --manifest catalog.toml --destination catalog-copy --json\n  af catalog test --source . --json"
    )]
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    /// Plan, run, and work a review Campaign
    #[command(
        long_about = "Plan, run, and work a review Campaign.\n\n\
A Campaign reviews one Subject (a diff or the working tree) against committed `.af/` policy. \
`plan` is token-free. `run` executes the pipeline inside sandboxes and folds results into the \
ledger; the remaining commands read or resolve that ledger. Flags given directly to `af review` \
are shorthand for `af review run`.",
        after_long_help = "Examples:\nHEAD must contain trusted, committed `.af/`; make candidate changes first.\nTo execute, also supply --provider NODE=ID for every model Worker required by that policy.\n  af review plan --policy-rev HEAD --base HEAD --uncommitted --json\n  af review --policy-rev HEAD --base HEAD --uncommitted\n  af review run --campaign pr-42 --policy-rev HEAD --base HEAD --uncommitted --heavy\n  af review ledger --campaign pr-42\n  af review report --campaign pr-42 --format md",
        override_usage = "af review <COMMAND>\n       af review [--heavy] [RUN OPTIONS]   (shorthand for `af review run`)",
        args_conflicts_with_subcommands = true,
        subcommand_negates_reqs = true,
        arg_required_else_help = true
    )]
    Review(ReviewNamespace),
    /// Inspect and preflight the configured model providers
    #[command(
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Inspect and preflight the configured model providers.\n\n\
Providers are named in the user configuration (`~/.config/af/providers.toml`); `status` reads \
that registry and the machine's harness logins without contacting any model. `models` lists \
each Provider's model catalog and effort levels, token-free, from the harness's own protocol. \
`doctor` runs a bounded, charged preflight for the bindings you name.\n\n\
Results go to stdout (add --json to `status`, `models` and `setup` for versioned documents); \
progress, warnings and diagnostics go to stderr. Exit codes across this namespace: 0 success, 1 \
unclassified failure, 2 usage error, 3 human action required, 4 Provider CLI missing or \
cannot start, 5 registry conflict, 6 authentication failed, 7 optional usage or a model catalog \
unavailable.",
        after_long_help = "Examples:\n  af provider status\n  af provider models --resolve 'Fable 5.1/xhigh'\n  af provider setup codex-main --kind codex\n  af provider doctor --provider correctness=claude-code"
    )]
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Generate or validate `.af/` review authority for a repository
    #[command(
        long_about = "Generate or validate `.af/` review authority for a repository.\n\n\
Behavior:\n\
  * Without .af/: preview a deterministic multi-review scaffold; --apply atomically creates it.\n\
  * With .af/: validate the selected pipeline, exact pins, Worker packages, graph, and Gates.\n\
  * --refresh-lock: explicitly re-pin the selected pipeline, every pipeline a route or the \
oversized policy names, and the Worker packages they reference; it drops a pin whose \
`.af/workers/NAME` package or `.af/pipelines/NAME.toml` file no longer exists.\n\
  * .af/af.lock records the af release that wrote it and the archive digest of that release for \
every target. Inside such a project any `af` on PATH runs that release, installing it on demand \
only when the bytes match; --refresh-lock re-pins to the running release, and --af VERSION runs \
the command under another installed-or-installable release, which is how a pin moves forward. A \
build without an install receipt (a source build) pins nothing.\n\n\
The command never calls a model, executes a Gate, reads credentials, creates Campaign state, \
fetches a PR, commits, pushes, comments, or overwrites an existing .af/ directory.\n\n\
Trusting configured Worker authority and intentionally running `af review run` or `af task \
start` authorizes delivery of each Worker's declared inputs for all Attempts and later Rounds or \
stages of that Campaign or Task. Afactory does not ask for per-call confirmation.\n\n\
Review Campaigns are light by default: one closed Round, then fix concrete Findings and run the \
deterministic project gate. Do not start another Campaign. Use `--heavy` only when a human \
explicitly requests convergence review, and repeat that explicit mode when resuming it.",
        after_long_help = "Runner profiles:\n  mixed   correctness = Claude Opus 5.5/high; architecture = GPT-6 Sol/high (default)\n  claude  both Workers = Claude Opus 5.5/high\n  codex   both Workers = GPT-6 Sol/high\n\nGate discovery prefers `make check`, then `scripts/verify.sh`, Rust, Go, or a package-manager test script. If none is unambiguous, pass a trusted literal.\n\nExamples:\n  af onboard\n  af onboard --gate 'check=make check' --apply\n  af onboard --runner mixed --apply\n  af onboard --refresh-lock\n  af onboard --refresh-lock --af 0.9.0     move the pin to 0.9.0"
    )]
    Onboard(OnboardArgs),
    /// Start, inspect, and deliver an implement Task
    #[command(
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Start, inspect, and deliver an implement Task.\n\n\
A Task file (`--file`) names the Task's ID, kind, goal and limits. The Pipeline and Worker \
packages it runs are pinned in the committed `.af/task-catalog.toml`; `af catalog init --profile software \
--destination DIR` creates a new starter directory with a working catalog and runnable Task \
files. The Task runs over a captured source Snapshot and ends at a verified or unverified \
derived Snapshot. Nothing is written back to the repository unless you `deliver` it, to a new \
local branch and worktree, after confirming the Task ID.",
        after_long_help = "Examples:\n  af task start --file ../ticket.json\n  af task run TASK_ID --confirm-plan PLAN_ID\n  af task list --json\n  af task show TASK_ID\n  af task deliver TASK_ID --repo . --branch af/TASK_ID --worktree ../TASK_ID --confirm TASK_ID"
    )]
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Show this repository's Project, measure it into Surveys, and manage its Workspaces
    #[command(
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Show this repository's Project, measure it into Surveys, and manage its \
Workspaces.\n\n\
A Project is the identity of one repository across every clone and worktree: `sha256:` + \
hex(SHA-256(`af/project-id@1\\0` + repository_id)), where repository_id is the sorted root \
commits every Snapshot origin already carries; never a path or a remote URL. Its Project Store \
lives under `$XDG_STATE_HOME/af/project/<hex>/`. A Workspace is one registered checkout of the \
Project, with its own Task Store, bindings file and Task-file directory, under one label; the \
paths are kept on this machine only, in the Project Store's `workspaces.toml`.\n\n\
Registration is lazy and token-free: `af onboard --apply`, `af task plan`, `af task start`, \
`af review run` and every `af project` command but `brief` register the Project and the invoking checkout \
(origin `adopted`, labelled by its directory name, its Store the default Task Store). A shallow \
clone is refused until `git fetch --unshallow`; a repository without commits has no Project. \
`af task deliver` registers the worktree it creates (origin `delivered`, labelled by the Task \
ID). `--workspace LABEL` on the `af task` commands that take `--state` selects a Workspace's \
Store, and on `plan` and `start` its bindings file.\n\n\
A Survey (`af/ProjectSurvey@2`) is one kernel-measured description of the Project at its head: \
identity, tree, history, contributors, decisions, vocabulary and checks, every number decimal \
text, bounded by the `[survey]` table of `.af/af.toml`. The token-free Surveyor records one at \
`af onboard --apply` and on `af project refresh`, in the Project Store, and reuses a complete \
Survey while its survey key (head, refs, policy digest, Surveyor version) is unchanged. It calls \
no Provider, runs no hook or project code, and never stores an author email.\n\n\
A Project Brief (`af/ProjectBrief@1`) is the bounded selection of one Survey that a pipeline's \
declared `project_brief` root port carries into the Workers it is wired to, and no other: a \
`[brief.<name>]` variant of `.af/af.toml` (built in: `orient` and `none`) rendered from the \
Survey a Task pinned when it was planned, whole sections dropped last-declared first until it \
fits `max_bytes`. `af project brief` prints the exact bytes a plan would bind, token-free.\n\n\
Never: writes inside a checkout, commits, pushes, or removes a Store. Results go to stdout \
(`--json` prints `af/project@1`, `af/project-workspaces@1`, `af/ProjectSurvey@2`, \
`af/project-history@2` or `af/project-brief-preview@1`); warnings go to stderr. Exit codes: 0 success, 1 failure (the message \
names the next action; `af/error@1` under --json), 2 usage error.",
        after_long_help = "Examples:\n  af project show\n  af project refresh\n  af project survey --json | jq .tree.files\n  af project history\n  af project brief --variant orient\n  af project workspaces --json\n  af project workspace new research --branch agent/research\n  af project workspace add ../afactory-wt-research --label research --state \"$W/state\"\n  af project workspace remove research\n  af project workspace remove research --delete-worktree\n  af task plan --workspace research --file ticket.json"
    )]
    Project {
        #[command(subcommand)]
        command: ProjectCli,
    },
    /// Show the effective configuration and where each value came from
    #[command(
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Show the effective configuration and where each value came from.\n\n\
Configuration is TOML, merged lowest to highest: built-in defaults, `/etc/af`, the user \
directory, every `.af/af.toml` in a directory above the repository, the project's `.af/af.toml`, \
`.af/af.local.toml`, then `AF_<TABLE>__<KEY>` environment overrides. Tables deep-merge, scalars \
last-wins, arrays replace. See `af help layers`.",
        after_long_help = "Examples:\n  af config show\n  af config show --origin\n  af config show --json | jq .self\n  af config edit --layer user"
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Analyze project economics, or install, update, roll back, and remove af itself
    #[command(
        name = "self",
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Analyze project economics, or install, update, roll back, and remove af itself.\n\n\
`self optimize` is project-scoped: it dispatches through the project pin, captures only declared \
history, and previews a report-only common Task. Binary-management subcommands remain outside \
project dispatch.\n\n\
Installed versions live under `$XDG_DATA_HOME/af/versions/<v>/` with a receipt each; the \
default is the `~/.local/bin/af` symlink. A project whose `.af/af.lock` pins another version \
is run by that version: `af` execs it, installing it on demand when the archive matches the \
digest the lock records. Outside a lock, releases are verified against their signed \
`SHA256SUMS`. Updates are checked by a detached, rate-limited child and applied by the policy \
in `[self]` (see `af help self`).\n\n\
An explicit `af self install V` run from a cargo/source-installed af at the default path replaces \
that running executable with the verified release and adopts the self-managed layout. Other \
binary-management commands never touch an unmanaged binary, and install never replaces an \
unrelated file at that path. It never stores a token, changes the version a pinned project runs, \
or activates a release older than 0.8.0 (the oldest supported release).",
        after_long_help = "Examples:\n  af self status\n  af self update --check\n  af self update\n  af self rollback\n  af self setup-shell --write"
    )]
    SelfCmd {
        #[command(subcommand)]
        command: SelfCommand,
    },
    /// Show what af keeps on this machine against its storage budget, and reclaim it
    #[command(
        args_conflicts_with_subcommands = true,
        long_about = "Show what af keeps on this machine against its storage budget, and reclaim it.\n\n\
af keeps at most `[storage] max_bytes` (20 GiB by default) between runs: warm build keys, warm \
Workspaces, review campaign Stores, Task Stores (in the state root and registered elsewhere), \
Project Stores (counted, never evicted), each Survey a Project Store holds (collected one at a time behind a tombstone, never the newest complete one) and installed versions. `af storage` prints, per kind, the entries, their bytes and their oldest and \
newest use; the Stores this release cannot read, with the reason; the total against the budget; \
and the free bytes against `[storage] min_free_bytes`, the floor below which af refuses to start a \
check or a Worker Attempt. It reads only; `af storage prune` previews the sweep that runs after \
every `af task run` and `af review run`, and `af storage prune --apply` performs it.\n\n\
`[storage]` is read only from machine layers (built-in, system, user, environment); a \
repository's `.af/af.toml` cannot change it. See `af help storage`.",
        after_long_help = "Examples:\n  af storage\n  af storage --json | jq .total_bytes\n  af storage prune\n  af storage prune --apply\n  AF_STORAGE__MAX_BYTES=10GiB af storage prune --apply"
    )]
    Storage {
        /// Machine-readable af/storage@4
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        command: Option<StorageCommand>,
    },
    /// Show what Telemetry recorded on this machine: its runs, span trees, live tail and ledger
    #[command(
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Show what Telemetry recorded on this machine: its runs, span trees, live tail and ledger.\n\n\
Telemetry records what af does while it works — spans, events and, at `content`, redacted \
content blobs — in its own store under the state root's `telemetry/`, never in a Task or \
Campaign Store. The kernel never reads it: identity, replay, the Ledger, budgets, admission and \
every Worker input are the same whether it is on, off or deleted (ADR-0153). `[telemetry] \
level` is `off`, `metrics`, `spans` (the default) or `content`, set from machine layers only; \
see `af help telemetry`.\n\n\
`status`, `runs`, `show`, `config`, `tail` and `ledger` read; `reindex` recovers crashed writers' segments and \
rebuilds the index; `gc --apply` evicts telemetry under the Storage Budget's rules. None records \
telemetry, contacts a network or reads a Store. Exit codes: 0 success, 1 the telemetry \
directory cannot be read, the index is in use or a run is unknown, 2 a wrong command line or a \
refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry status\n  af telemetry runs --since 7d\n  af telemetry show RUN_ID\n  af telemetry config RUN_ID\n  af telemetry tail\n  af telemetry ledger RUN_ID\n  af telemetry gc\n  af telemetry status --json | jq .segments\n  af telemetry reindex\n  af telemetry status --dir ./copied-telemetry\n  AF_TELEMETRY__LEVEL=content af task run TASK_ID"
    )]
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCommand,
    },
    /// Print a shell completion script (values complete live from the binary)
    #[command(
        long_about = "Print a shell completion script.\n\n\
The script is thin: it asks the running `af` for candidates, so campaign names, task IDs, \
config keys, and installed versions complete from real state and the script never goes stale \
when the binary updates. `af self setup-shell --write` installs it in the shell's autoload \
directory.",
        after_long_help = "Examples:\n  af completions fish > ~/.config/fish/completions/af.fish\n  af completions zsh > ~/.local/share/zsh/site-functions/_af\n  source <(af completions bash)"
    )]
    Completions {
        /// The shell to generate for
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Explain a topic (config, layers, environment, exit-codes, json, self, storage, telemetry) or a command
    #[command(
        long_about = "Explain a topic or a command.\n\n\
Topics: config, layers, environment, exit-codes, json, self, storage, telemetry. Anything else \
is treated \
as a command path, so `af help review run` equals `af review run --help`.",
        after_long_help = "Examples:\n  af help layers\n  af help exit-codes\n  af help review ledger"
    )]
    Help {
        /// A topic name or a command path
        #[arg(value_name = "TOPIC|COMMAND")]
        words: Vec<String>,
    },
}

// ------------------------------------------------------------------------------------------
// review

#[derive(Debug, Args)]
pub(crate) struct ReviewNamespace {
    #[command(flatten)]
    pub(crate) run: RunArgs,
    #[command(subcommand)]
    pub(crate) command: Option<ReviewCommand>,
}

/// Selector, mode, budget, and output options shared by `plan`, `run`, `render`, and `provider
/// doctor`.
#[derive(Debug, Args, Clone)]
pub(crate) struct RunArgs {
    /// Versioned Review Task file, executed through the shared Task runtime
    #[arg(long = "file", value_name = "FILE", help_heading = "Selector", conflicts_with_all = ["pipeline", "campaign", "base", "candidate", "focus", "node", "heavy", "restart_round", "provider", "provider_admission_tokens", "provider_admission_wall_ms", "git_timeout_secs"])]
    pub(crate) task_file: Option<PathBuf>,
    /// Explicit local Worker packages, slot replacements and Provider aliases
    #[arg(
        long,
        value_name = "FILE",
        requires = "task_file",
        help_heading = "Selector"
    )]
    pub(crate) bindings: Option<PathBuf>,
    /// Repository to review
    #[arg(
        long,
        value_name = "DIR",
        default_value = ".",
        help_heading = "Selector"
    )]
    pub(crate) repo: PathBuf,
    /// Pipeline definition, relative to the repository [default: .af/pipelines/review.toml, or
    /// the pipeline the project's routes select from the changed paths]
    #[arg(long, value_name = "FILE", help_heading = "Selector")]
    pub(crate) pipeline: Option<PathBuf>,
    /// Explicit Campaign state directory (outside the repository)
    #[arg(long, value_name = "DIR", help_heading = "Selector")]
    pub(crate) state: Option<PathBuf>,
    /// Campaign name; state lives under XDG state by an opaque id
    #[arg(long, value_name = "NAME", help_heading = "Selector", add = ArgValueCompleter::new(complete_campaign))]
    pub(crate) campaign: Option<String>,
    /// Policy revision: the commit whose `.af/` governs this Campaign
    #[arg(long, value_name = "REV", help_heading = "Selector")]
    pub(crate) policy_rev: Option<String>,
    /// Diff base revision
    #[arg(long, value_name = "REV", help_heading = "Selector")]
    pub(crate) base: Option<String>,
    /// Candidate revision to review (default: HEAD)
    #[arg(
        long,
        value_name = "REV",
        help_heading = "Selector",
        conflicts_with = "uncommitted"
    )]
    pub(crate) candidate: Option<String>,
    /// Review the working tree (including unstaged changes) against HEAD
    #[arg(long, help_heading = "Selector")]
    pub(crate) uncommitted: bool,
    /// Free-text focus handed to every reviewer
    #[arg(long, value_name = "TEXT", help_heading = "Selector")]
    pub(crate) focus: Option<String>,
    /// `render`: the Worker node whose exact input to compose
    #[arg(long, value_name = "NODE", help_heading = "Selector")]
    pub(crate) node: Option<String>,
    /// The pipeline's complete convergence policy, instead of the default single light Round
    #[arg(long, help_heading = "Mode")]
    pub(crate) heavy: bool,
    /// Abandon the open Round and start a fresh one
    #[arg(long, help_heading = "Mode")]
    pub(crate) restart_round: bool,
    /// Bind a pipeline node to a provider from the registry
    #[arg(long, value_name = "NODE=PROVIDER_ID", action = ArgAction::Append, help_heading = "Providers")]
    pub(crate) provider: Vec<String>,
    /// Tokens reserved for each new common Review Provider admission [new capture: 32768]
    #[arg(
        long,
        value_name = "N",
        requires = "provider_admission_wall_ms",
        help_heading = "Budget"
    )]
    pub(crate) provider_admission_tokens: Option<u64>,
    /// Milliseconds reserved per admission; supply both bounds, or omit both on resume
    #[arg(
        long,
        value_name = "N",
        requires = "provider_admission_tokens",
        help_heading = "Budget"
    )]
    pub(crate) provider_admission_wall_ms: Option<u64>,
    /// Wall-clock budget in seconds. Without --file (default routed run, --campaign or --pipeline):
    /// per reviewer Attempt (default 1800; pinned in the Campaign manifest as
    /// reviewer_timeout_seconds), not a bound on the
    /// whole run. With --file: a cap on the whole Task, the lower of this flag and the file's
    /// limits.wall_ms
    #[arg(long, value_name = "N", help_heading = "Budget")]
    pub(crate) timeout_secs: Option<u64>,
    /// Wall-clock budget for each git operation
    #[arg(long, value_name = "N", help_heading = "Budget")]
    pub(crate) git_timeout_secs: Option<u64>,
    /// One JSON document on stdout instead of text
    #[arg(long, help_heading = "Output")]
    pub(crate) json: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct CampaignSelector {
    /// Campaign name
    #[arg(long, value_name = "NAME", help_heading = "Selector", add = ArgValueCompleter::new(complete_campaign))]
    pub(crate) campaign: String,
    /// Explicit Campaign state directory
    #[arg(long, value_name = "DIR", help_heading = "Selector")]
    pub(crate) state: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ActorArgs {
    /// Who records this transition (default: the local user)
    #[arg(long, value_name = "ACTOR", help_heading = "Record")]
    pub(crate) actor: Option<String>,
    /// Evidence artifact ids supporting the transition
    #[arg(long, value_name = "ID", action = ArgAction::Append, help_heading = "Record")]
    pub(crate) evidence: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ReviewCommand {
    /// Execute the pipeline and fold results into the ledger
    #[command(
        long_about = "Execute the pipeline over the selected Subject and fold every result into the \
Campaign ledger.\n\nEach reviewer runs in a private sandbox with an exact, role-scoped input and \
a token budget reserved before dispatch; results are admitted in canonical order. The exit code \
is the verdict: 0 pass, 3 fail, 4 incomplete.\n\nNever: mutates the repository, commits, pushes, \
or contacts anything but the bound providers.",
        override_usage = "af review run [--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  af review run --uncommitted\n  af review run --campaign pr-42 --policy-rev main --base main --candidate HEAD\n  af review run --heavy --provider correctness=claude-code --json"
    )]
    Run(RunArgs),
    /// Show what a review would run, token-free, without Campaign state
    #[command(
        long_about = "Resolve policy, pipeline, Workers, providers, and the Subject and print the \
plan.\n\nToken-free and stateless: no sandbox, no model call, no Campaign directory. The plan is \
exactly what `run` would admit, so it is the right pre-flight for agents and CI.",
        override_usage = "af review plan [--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  af review plan\n  af review plan --uncommitted --json"
    )]
    Plan(RunArgs),
    /// Compose one Worker's exact input, token-free, without Campaign state
    #[command(
        long_about = "Compose the exact input one Worker node would receive — the bytes, their \
transport, and every artifact the role-scoped manifest names — without a sandbox, a model call, \
or Campaign state. This is how a human audits what a Worker sees.",
        override_usage = "af review render --node <NODE> [--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  af review render --node correctness\n  af review render --node correctness --uncommitted --json"
    )]
    Render(RunArgs),
    /// List the ledger: findings, dispositions, and open demands
    #[command(after_long_help = "Examples:\n  af review ledger --campaign pr-42 --long")]
    Ledger {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Full detail per finding
        #[arg(long, help_heading = "Output")]
        long: bool,
    },
    /// Show one ledger entry by key
    #[command(after_long_help = "Examples:\n  af review show --campaign pr-42 F-01H…")]
    Show {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding, demand, or proposal key
        key: String,
    },
    /// Export a verified patch proposal, by id or by the finding it fixes
    #[command(
        group = ArgGroup::new("target").required(true).args(["proposal", "finding"]),
        after_long_help = "Examples:\n  af review export --campaign pr-42 P-01H…\n  af review export --campaign pr-42 --finding F-01H… --allow-stale"
    )]
    Export {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Proposal id
        proposal: Option<String>,
        /// Export the proposal that fixes this finding
        #[arg(long, value_name = "FINDING_ID")]
        finding: Option<String>,
        /// Export even when the proposal's base is no longer the Campaign's
        #[arg(long)]
        allow_stale: bool,
    },
    /// Render the Campaign report
    #[command(
        after_long_help = "Examples:\n  af review report --campaign pr-42\n  af review report --campaign pr-42 --format json"
    )]
    Report {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Output format
        #[arg(long, value_enum, default_value_t = ReportFormatArg::Md, help_heading = "Output")]
        format: ReportFormatArg,
    },
    /// List Campaigns under the state root
    #[command(
        after_long_help = "Examples:\n  af review campaigns\n  af review campaigns --format json"
    )]
    Campaigns {
        /// State root to enumerate (default: XDG state)
        #[arg(long, value_name = "DIR")]
        state_root: Option<PathBuf>,
        /// Output format
        #[arg(long, value_enum, default_value_t = ListFormatArg::Text, help_heading = "Output")]
        format: ListFormatArg,
        /// Walk each state directory for its size on disk (seconds on a large root)
        #[arg(long)]
        sizes: bool,
    },
    /// Reclaim Campaign state: list what would go; remove it only with --apply
    #[command(
        long_about = "List the Campaigns whose newest store write is at least --older-than days old \
(never the --keep newest) and how much they hold on disk. Only --apply removes them, whole \
Campaign directories at a time; directories the enumeration cannot read are reported and left \
alone, and nothing inside a kept Campaign is ever touched.",
        after_long_help = "Examples:\n  af review gc --older-than 14 --keep 5\n  af review gc --older-than 14 --keep 5 --apply\n  af review gc --keep 10 --json"
    )]
    Gc {
        /// State root to reclaim under (default: XDG state)
        #[arg(long, value_name = "DIR")]
        state_root: Option<PathBuf>,
        /// Campaigns whose newest store write is at least this many days old
        #[arg(long, value_name = "DAYS", required_unless_present = "keep")]
        older_than: Option<u64>,
        /// Never touch the newest N Campaigns
        #[arg(long, value_name = "N")]
        keep: Option<usize>,
        /// Remove the listed Campaign directories (default: preview only)
        #[arg(long)]
        apply: bool,
        /// Machine-readable af/review-gc@1
        #[arg(long, help_heading = "Output")]
        json: bool,
    },
    /// Resolve a finding without a fix: rejected, or tracked as wontfix
    #[command(
        long_about = "Record a non-fixed resolution for a finding.\n\n`rejected` disputes the \
finding; `wontfix-tracked` accepts it under a severity ceiling, a tracking reference, and a \
policy-time expiry, and requires --max-severity, --tracking, and --expires-at-policy-time. \
Non-fixed resolutions are scoped and challengeable.",
        after_long_help = "Examples:\n  af review resolve --campaign pr-42 F-01H… rejected --policy main --reason \"not reachable\"\n  af review resolve --campaign pr-42 F-01H… wontfix-tracked --policy main --reason \"tracked\" --max-severity major --tracking JIRA-1 --expires-at-policy-time 12"
    )]
    Resolve {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding key
        key: String,
        /// Resolution status: rejected or wontfix-tracked (a fix is proven with attest-change)
        #[arg(value_name = "rejected|wontfix-tracked")]
        status: String,
        /// Policy revision the resolution is recorded under
        #[arg(long, value_name = "REV")]
        policy: String,
        /// Why
        #[arg(long, value_name = "TEXT")]
        reason: String,
        #[command(flatten)]
        actor: ActorArgs,
        /// wontfix-tracked: highest severity this resolution may cover
        #[arg(
            long,
            value_enum,
            value_name = "SEVERITY",
            help_heading = "wontfix-tracked"
        )]
        max_severity: Option<SeverityArg>,
        /// wontfix-tracked: external tracking reference
        #[arg(long, value_name = "REF", help_heading = "wontfix-tracked")]
        tracking: Option<String>,
        /// wontfix-tracked: policy-time tick at which the resolution expires
        #[arg(long, value_name = "TICK", help_heading = "wontfix-tracked")]
        expires_at_policy_time: Option<u64>,
    },
    /// Attest that a region changed in response to a finding
    #[command(
        after_long_help = "Examples:\n  af review attest-change --campaign pr-42 F-01H… --region src/lib.rs:10-20 --reason \"guarded\""
    )]
    AttestChange {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding key
        finding: String,
        /// Changed region, PATH or PATH:START-END
        #[arg(long, value_name = "PATH[:START-END]", action = ArgAction::Append)]
        region: Vec<String>,
        /// Why
        #[arg(long, value_name = "TEXT")]
        reason: String,
        #[command(flatten)]
        actor: ActorArgs,
    },
    /// Record a verification of an attested fix
    #[command(
        group = ArgGroup::new("outcome").required(true).args(["positive", "negative"]),
        after_long_help = "Examples:\n  af review verify-fix --campaign pr-42 F-01H… A-01H… --policy main --reason \"reproduced clean\" --positive"
    )]
    VerifyFix {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding key
        finding: String,
        /// Attestation id
        attestation: String,
        /// Policy revision the verification is recorded under
        #[arg(long, value_name = "REV")]
        policy: String,
        /// Why
        #[arg(long, value_name = "TEXT")]
        reason: String,
        /// The fix holds
        #[arg(long)]
        positive: bool,
        /// The fix does not hold
        #[arg(long)]
        negative: bool,
        /// Who verified (default: the local user)
        #[arg(long, value_name = "ACTOR", help_heading = "Record")]
        verifier: Option<String>,
        /// Evidence artifact ids supporting the verification
        #[arg(long, value_name = "ID", action = ArgAction::Append, help_heading = "Record")]
        evidence: Vec<String>,
    },
    /// Challenge a non-fixed resolution
    #[command(
        after_long_help = "Examples:\n  af review challenge-resolution --campaign pr-42 F-01H… --kind new-evidence --reason \"repro attached\" --evidence E-01H…"
    )]
    ChallengeResolution {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding key
        finding: String,
        /// Ground for the challenge
        #[arg(long, value_enum)]
        kind: ChallengeKindArg,
        /// Why
        #[arg(long, value_name = "TEXT")]
        reason: String,
        #[command(flatten)]
        actor: ActorArgs,
    },
    /// Advance the Campaign's policy clock
    #[command(subcommand_required = true, arg_required_else_help = true)]
    PolicyTime {
        #[command(subcommand)]
        command: PolicyTimeCommand,
    },
    /// Group one finding under another
    #[command(after_long_help = "Examples:\n  af review group --campaign pr-42 F-02H… F-01H…")]
    Group {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding to fold in
        from: String,
        /// Finding that absorbs it
        into: String,
    },
    /// Undo a grouping
    Ungroup {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding to release
        from: String,
        /// Finding it was grouped under
        into: String,
    },
    /// Attach or apply evidence to a demand
    #[command(subcommand_required = true, arg_required_else_help = true)]
    Evidence {
        #[command(subcommand)]
        command: EvidenceCommand,
    },
    /// Waive a demand
    #[command(subcommand_required = true, arg_required_else_help = true)]
    Demand {
        #[command(subcommand)]
        command: DemandCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum PolicyTimeCommand {
    /// Advance the policy clock to a tick
    #[command(
        after_long_help = "Examples:\n  af review policy-time advance --campaign pr-42 12 --reason \"sprint end\""
    )]
    Advance {
        #[command(flatten)]
        selector: CampaignSelector,
        /// The new tick (monotonic)
        tick: u64,
        /// Why
        #[arg(long, value_name = "TEXT")]
        reason: String,
        /// Who records this (default: the local user)
        #[arg(long, value_name = "ACTOR", help_heading = "Record")]
        actor: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum EvidenceCommand {
    /// Attach a file as evidence for a demand
    #[command(
        after_long_help = "Examples:\n  af review evidence add --campaign pr-42 D-01H… ./bench.txt"
    )]
    Add {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Demand id
        demand: String,
        /// File to attach
        file: PathBuf,
        /// Who records this (default: the local user)
        #[arg(long, value_name = "ACTOR", help_heading = "Record")]
        actor: Option<String>,
    },
    /// Mark a demand satisfied by attached evidence
    #[command(
        after_long_help = "Examples:\n  af review evidence satisfy --campaign pr-42 D-01H… E-01H… --policy main --reason \"bench attached\""
    )]
    Satisfy {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Demand id
        demand: String,
        /// Evidence id
        evidence: String,
        /// Policy revision the satisfaction is recorded under
        #[arg(long, value_name = "REV")]
        policy: String,
        /// Why
        #[arg(long, value_name = "TEXT")]
        reason: String,
        /// Allow evidence already used for another demand
        #[arg(long)]
        admit_reuse: bool,
        /// Who records this (default: the local user)
        #[arg(long, value_name = "ACTOR", help_heading = "Record")]
        actor: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum DemandCommand {
    /// Waive a demand under a policy revision
    #[command(
        after_long_help = "Examples:\n  af review demand waive --campaign pr-42 D-01H… --policy main --reason \"out of scope\""
    )]
    Waive {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Demand id
        demand: String,
        /// Policy revision the waiver is recorded under
        #[arg(long, value_name = "REV")]
        policy: String,
        /// Why
        #[arg(long, value_name = "TEXT")]
        reason: String,
        /// Who records this (default: the local user)
        #[arg(long, value_name = "ACTOR", help_heading = "Record")]
        actor: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ReportFormatArg {
    Md,
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ListFormatArg {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SeverityArg {
    Minor,
    Major,
    Blocker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ChallengeKindArg {
    NewEvidence,
    HigherSeverity,
    OutsideScope,
    Expired,
}

// ------------------------------------------------------------------------------------------
// provider · onboard · task

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum ProviderCommand {
    /// Show the provider registry and each harness login, without a model call
    #[command(
        long_about = "Show the provider registry and each harness login, without a model call.\n\n\
Fast by default: the registry, a token-free check that each context's CLI accepts af's \
invocation, and one bounded authentication check per context. Subscription \
and quota probes are opt-in behind --usage, and an unavailable usage probe never demotes an \
authenticated Provider — it exits 7 and says so. End-to-end usability stays with the charged \
`af provider doctor`.\n\n\
A Provider whose official CLI is missing, not executable, or exits non-zero on its own version \
check is reported as an installation failure, with the CLI's own first error line and the fix \
it suggests on stderr. One whose help page does not declare every option af passes is reported \
as incompatible, with its version, the missing options and the upgrade command on stderr.\n\n\
Exit codes: 0 fine, 4 a Provider CLI cannot start or is incompatible, 7 optional usage \
unavailable.",
        after_long_help = "Examples:\n  af provider status\n  af provider status --json\n  af provider status --usage"
    )]
    Status {
        /// Emit the versioned `af/provider-status@2` document instead of the table
        #[arg(long)]
        json: bool,
        /// Also probe subscription and quota windows (slower, and may be unavailable)
        #[arg(long)]
        usage: bool,
    },
    /// List each Provider's model catalog and effort levels, without a model call
    #[command(
        long_about = "List each Provider's model catalog and effort levels, without a model call.\n\n\
For every registered Provider, or the named ones, af reads the catalog its harness offers \
through the harness's own local protocol, under that Provider's auth directory and the bounded \
probe environment `af provider status` uses, and never starts a turn: Claude Code answers the \
`list_models` control request of a stream-json session af closes before any turn, Codex answers \
`model/list` on a bare `codex app-server`. Each Provider gets a header with the source, the \
harness version and the freshness (Codex: `fetched_at` of `models_cache.json` when readable, \
else live), one row per exact model ID with its display name, the aliases that resolve to it \
and its effort levels with the default marked, and a sentence saying what the list proves: af \
recognises these spellings for this Provider. It is not every ID the API accepts, and whether \
this account can run one is proven only by the charged `af provider doctor`.\n\n\
--resolve reads a human spelling (an alias, a display name, an exact ID, or a family and \
version such as `Fable 5.1` or `GPT-6 Sol`) with an optional /EFFORT, says what it resolves to \
and whether the effort level is listed, and prints the configuration to copy: the reviewer.toml \
[runner] args, the worker.toml [runner] block and the bindings line. af binds exact IDs: an alias \
is never bound, and an effort level a model does not list is never substituted.\n\n\
Spends no model tokens and reads no credential.\n\n\
Exit codes: 0 every catalog listed, 4 a Provider CLI cannot start or is incompatible, 7 a \
catalog is unavailable (the static rules are printed and nothing is invented).",
        after_long_help = "Examples:\n  af provider models\n  af provider models claude-work --json\n  af provider models claude-work --resolve 'Fable 5.1/xhigh'\n  af provider models codex-main --resolve 'GPT-6 Sol/ultra'"
    )]
    Models {
        /// Provider IDs to list (default: every registered Provider)
        #[arg(value_name = "ID")]
        ids: Vec<String>,
        /// Emit the versioned `af/provider-models@1` document instead of the table
        #[arg(long)]
        json: bool,
        /// Resolve a human model spelling, with an optional /EFFORT, to an exact ID
        #[arg(long, value_name = "SPELLING[/EFFORT]")]
        resolve: Option<String>,
    },
    /// Register one named machine-local Provider, optionally opting in to its official login
    #[command(
        long_about = "Register one named machine-local Provider, optionally opting in to its \
official login.\n\n\
Verifies that the chosen auth directory is authenticated and records only its kind and path in \
the machine-local registry; credentials remain owned by the Provider CLI. An already \
authenticated context is registered without starting any login.\n\n\
Without --login, a context that has no login is never logged in for you: setup prints the \
human-action-required result and the exact command to run. With --login, setup starts the \
official Provider CLI login only when this process owns an interactive terminal on stdin, \
stdout and stderr, because the OAuth URL and authorization code that login prints are \
credentials in transit and must never reach a pipe, chat, an agent transcript, or logs.\n\n\
Exit codes: 0 registered, 3 human action required, 4 Provider CLI missing, cannot start or \
incompatible, 5 \
registry conflict, 6 authentication failed. Re-running setup for the same binding is safe.",
        after_long_help = "Examples:\n  af provider setup codex-main --kind codex\n  af provider setup codex-main --kind codex --json\n  af provider setup claude-work --kind claude --auth-dir /secure/claude-work --login"
    )]
    Setup {
        /// Stable ID used by --provider NODE=ID
        id: String,
        /// Provider CLI family
        #[arg(long, value_enum)]
        kind: ProviderKindArg,
        /// Auth directory (defaults to the active CLI directory)
        #[arg(long, value_name = "DIR")]
        auth_dir: Option<PathBuf>,
        /// Opt in to starting the official Provider CLI login in this private terminal
        #[arg(long)]
        login: bool,
        /// Emit the versioned non-interactive `af/provider-setup@1` document
        #[arg(long, conflicts_with = "login")]
        json: bool,
    },
    /// Register one named machine-local Provider auth context
    #[command(
        long_about = "Register one named machine-local Provider auth context.\n\n\
The registry stores only the Provider kind and auth directory, never credentials. When \
--auth-dir is omitted, af uses the active CLI directory (CLAUDE_CONFIG_DIR or ~/.claude; \
CODEX_HOME or ~/.codex). Existing entries and duplicate auth contexts are never overwritten, \
and no login is ever started.\n\n\
Exit codes: 0 registered, 5 registry conflict.",
        after_long_help = "Examples:\n  af provider add codex-main --kind codex\n  af provider add claude-work --kind claude --auth-dir /secure/claude-work"
    )]
    Add {
        /// Stable ID used by --provider NODE=ID
        id: String,
        /// Provider CLI family
        #[arg(long, value_enum)]
        kind: ProviderKindArg,
        /// Auth directory (defaults to the active CLI directory)
        #[arg(long, value_name = "DIR")]
        auth_dir: Option<PathBuf>,
    },
    /// Remove named Providers from the machine-local registry
    #[command(
        long_about = "Remove named Providers from the machine-local registry.\n\n\
Only the registry entries go: each auth directory, and the login its Provider CLI keeps there, \
is left untouched, so `af provider add` or `setup` can register the context again. The previous \
registry is preserved beside the new one.\n\n\
An entry whose auth directory no longer exists makes the whole registry invalid, and removing \
it is the repair; name every stale entry in one command. An ambient ID (`claude-ambient`, \
`codex-ambient`) is a discovery label, not a registry entry, and cannot be removed.\n\n\
Exit codes: 0 removed, 1 an ID is not registered or the registry cannot be rewritten.",
        after_long_help = "Examples:\n  af provider remove codex-main\n  af provider remove claude-work codex-work"
    )]
    Remove {
        /// Registered Provider IDs to remove
        #[arg(value_name = "ID", required = true, num_args = 1..)]
        ids: Vec<String>,
    },
    /// Explicit, separately permissioned private-host login recovery (no model calls)
    #[command(
        long_about = "Recover one exact native Provider auth context through a trusted \
private host pipe pair (Linux with /proc/self/fd). The host must approve this login and deliver its challenge privately \
to the requesting human. OAuth material never appears in ordinary output. The owner process \
remains running across agent turns; the host must keep it and its private channel alive. \
Completion is authenticated_unverified, never proof of model usability or Task execution authority. \
Generic provider setup --login remains terminal-only."
    )]
    Auth {
        #[command(subcommand)]
        command: ProviderAuthCommand,
    },
    /// Validate and close an interrupted Provider registry publication
    #[command(
        long_about = "Validate and close an interrupted Provider registry publication.\n\n\
Checks the transaction marker, live registry, and preserved candidate/prior hashes before \
archiving the marker. It never guesses which version committed and never deletes recovery files."
    )]
    Recover,
    /// Run a bounded, charged preflight for the named bindings
    #[command(
        long_about = "Run a bounded, charged preflight for the named provider bindings.\n\n\
Resolves the review selector exactly as `af review plan` would, then sends each bound provider \
one fenced preflight operation and reports identity, model, and spend.",
        after_long_help = "Examples:\n  af provider doctor --provider correctness=claude-code"
    )]
    Doctor(RunArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProviderAuthCommand {
    /// Begin or reuse one login; a private host grant is required before the CLI starts
    Begin {
        id: String,
        #[arg(long, value_enum)]
        kind: ProviderKindArg,
        /// Exact existing or new native auth directory; ambient credentials are never selected
        #[arg(long, value_name = "DIR")]
        auth_dir: PathBuf,
        /// Dedicated inherited pipe from the trusted host; must be greater than 2
        #[arg(long)]
        host_read_fd: u32,
        /// Dedicated inherited pipe to the trusted host; must be greater than 2
        #[arg(long)]
        host_write_fd: u32,
        /// Host-owned login lifetime; not the provider's challenge expiry (30..900 seconds)
        #[arg(long, default_value_t = 600)]
        timeout_secs: u64,
    },
    /// Inspect closed non-secret state; interrupted owners never appear authenticated
    Status {
        id: String,
        #[arg(long, value_enum)]
        kind: ProviderKindArg,
        #[arg(long, value_name = "DIR")]
        auth_dir: PathBuf,
    },
    /// Cancel only the currently identified recovery session (idempotent)
    Cancel {
        id: String,
        #[arg(long, value_enum)]
        kind: ProviderKindArg,
        #[arg(long, value_name = "DIR")]
        auth_dir: PathBuf,
        #[arg(long)]
        recovery_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProviderKindArg {
    Claude,
    Codex,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct OnboardArgs {
    /// Repository to onboard
    #[arg(
        long,
        value_name = "DIR",
        default_value = ".",
        help_heading = "Selector"
    )]
    pub(crate) repo: PathBuf,
    /// Runner profile for the generated Workers
    #[arg(long, value_enum, default_value_t = RunnerArg::Mixed, help_heading = "Generate")]
    pub(crate) runner: RunnerArg,
    /// A Gate the review must pass, NAME=COMMAND (repeatable)
    #[arg(long, value_name = "NAME=COMMAND", action = ArgAction::Append, help_heading = "Generate")]
    pub(crate) gate: Vec<String>,
    /// Write the previewed bundle (absent-only; never overwrites)
    #[arg(long, help_heading = "Action", conflicts_with = "refresh_lock")]
    pub(crate) apply: bool,
    /// Repin the selected, routed and oversized pipelines and their Workers; drop stale pins
    #[arg(long, help_heading = "Action")]
    pub(crate) refresh_lock: bool,
    /// Run this command under release VERSION, installing it on demand (how a pin moves)
    #[arg(long, value_name = "VERSION", help_heading = "Action")]
    pub(crate) af: Option<String>,
    /// One JSON document on stdout instead of text
    #[arg(long, help_heading = "Output")]
    pub(crate) json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RunnerArg {
    Mixed,
    Claude,
    Codex,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CatalogCommand {
    /// Create supported Task and Worker starters in an absent directory
    Init {
        #[arg(long, default_value="document", value_parser=["document", "software", "planning", "all", "experiment", "report"])]
        profile: String,
        /// Existing minisign public key assigned to developer owner; enables the Planner
        #[arg(long)]
        developer_public_key: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Absent directory, as a safe path relative to --repo (an absolute path is refused)
        #[arg(long)]
        destination: String,
        #[arg(long)]
        json: bool,
    },
    /// Check exact Pipeline/Worker contract fixtures from a local Git revision without dispatch
    Test {
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value = "HEAD")]
        revision: String,
        #[arg(long, default_value = "catalog.toml")]
        manifest: String,
        /// Fixture path in the same Git commit (defaults beside the manifest)
        #[arg(long)]
        fixtures: Option<String>,
        #[arg(long, conflicts_with = "worker")]
        pipeline: Option<String>,
        #[arg(long)]
        worker: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print the digest a catalog pin records for one local Task package directory
    #[command(
        long_about = "Print the digest a catalog pin records for one local Task package \
directory: the `digest` of its `[packages.\"NAME\"]` entry in `.af/task-catalog.toml` or a \
shared catalog.\n\nThe output is one line, `sha256:` and 64 lowercase hexadecimal digits, and \
nothing else. Every regular file under the directory contributes its relative path and its \
bytes, the manifest included; file modes, timestamps, empty directories and the directory's own \
name or location do not. Task capture hashes the package's files in the captured Snapshot, so \
a file the Snapshot leaves out, such as an ignored one, changes this digest but not the \
captured one.\n\n\
The package is validated as Task capture validates it: exactly one `pipeline.toml`, \
`worker.toml` or `kind.toml` at its root whose name and exact version the full manifest parse \
accepts, regular files only, and capture's bounds of 4,096 files and 16 MiB. An absent \
directory, a symlink, an incomplete or malformed package prints nothing on stdout, says why on \
stderr and exits 1.\n\nNever: compares against or changes a catalog or pin, writes a file, runs a \
Worker, contacts a Provider or installs anything. A project's `af` pin never dispatches it to \
another release, and no update check follows it.",
        after_long_help = "Examples:\n  af catalog digest .af/task-packages/team/implementer\n  \
pin=$(af catalog digest .af/task-packages/team/implementer) || exit 1"
    )]
    Digest {
        /// Task package directory, the `path` its catalog pin names
        #[arg(value_name = "PACKAGE_DIR")]
        package: PathBuf,
    },
    /// Explicitly sync a Git catalog into an absent directory; runs no Workers or installers
    Sync {
        /// Local Git checkout or public HTTPS Git URL
        #[arg(long)]
        source: String,
        #[arg(long, default_value = "HEAD")]
        revision: String,
        #[arg(long, default_value = "catalog.toml")]
        manifest: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Absent directory, as a safe path relative to --repo (an absolute path is refused)
        #[arg(long)]
        destination: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum TaskCommand {
    /// Capture a Task and preview its plan before execution
    #[command(
        long_about = "Capture a Task and show its execution plan before any Worker runs.\n\nInspect the compact ASCII preview, or use `af task explain TASK_ID --tree` for the expanded hierarchy. Run with `--confirm-plan PLAN_ID` after reviewing the captured plan. `--execute` explicitly opts into immediate automation. JSON output also previews by default. Generated plans still require signed developer approval.\n\nExecution never writes to the original repository, commits, pushes, or delivers; see `af task deliver`.",
        after_long_help = "Examples:\n  af task start --file ../ticket.json\n  af task explain TASK_ID --tree\n  af task run TASK_ID --confirm-plan sha256:...\n  af task start --file ../ticket.json --execute --json"
    )]
    Start {
        /// Versioned Task JSON/TOML file, processed by the common Task runtime
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
        /// Explicitly execute immediately instead of stopping at the captured plan preview
        #[arg(long)]
        execute: bool,
        /// Capture local Worker tuning for this Task
        #[arg(long, value_name = "FILE")]
        bindings: Option<PathBuf>,
        /// Explicit machine-local read-only issue source accounts
        #[arg(long, value_name = "FILE")]
        source_bindings: Option<PathBuf>,
        /// Repository to work in
        #[arg(
            long,
            value_name = "DIR",
            default_value = ".",
            help_heading = "Selector"
        )]
        repo: PathBuf,
        /// Explicit state directory (outside the repository)
        #[arg(long, value_name = "DIR", help_heading = "Selector")]
        state: Option<PathBuf>,
        /// The Workspace whose Store and bindings file to use, by label (`af project workspaces`)
        #[arg(
            long,
            value_name = "LABEL",
            conflicts_with = "state",
            help_heading = "Selector"
        )]
        workspace: Option<String>,
        /// Source revision (default: HEAD)
        #[arg(
            long,
            value_name = "REV",
            default_value = "HEAD",
            help_heading = "Selector",
            conflicts_with = "uncommitted"
        )]
        authority: String,
        /// Start from the working tree instead of a revision
        #[arg(long, help_heading = "Selector")]
        uncommitted: bool,
        /// Positive wall-clock budget for the whole Task
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..), help_heading = "Budget")]
        timeout_secs: Option<u64>,
        /// One JSON document on stdout instead of text
        #[arg(long, help_heading = "Output")]
        json: bool,
    },
    /// Capture and compile a Task file without dispatching any Worker
    Plan {
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
        #[arg(long, value_name = "FILE")]
        bindings: Option<PathBuf>,
        /// Explicit machine-local read-only issue source accounts
        #[arg(long, value_name = "FILE", requires = "file")]
        source_bindings: Option<PathBuf>,
        #[arg(long, value_name = "DIR", default_value = ".")]
        repo: PathBuf,
        #[arg(long, value_name = "DIR")]
        state: Option<PathBuf>,
        /// The Workspace whose Store and bindings file to use, by label (`af project workspaces`)
        #[arg(long, value_name = "LABEL", conflicts_with = "state")]
        workspace: Option<String>,
        #[arg(long, default_value = "HEAD", conflicts_with = "uncommitted")]
        authority: String,
        #[arg(long)]
        uncommitted: bool,
        #[arg(long)]
        json: bool,
    },
    /// Capture an updated issue and replace its plan without executing Workers
    Refresh {
        task_id: String,
        /// Read a fresh local JSON/TOML issue (defaults to the captured project path)
        #[arg(long, value_name = "FILE", conflicts_with = "source_bindings")]
        source_file: Option<PathBuf>,
        /// Explicit machine-local account for the captured Jira selector
        #[arg(long, value_name = "FILE")]
        source_bindings: Option<PathBuf>,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Write exact outer-plan or pending experiment bytes for an external developer signature
    DecisionPayload {
        task_id: String,
        #[arg(long)]
        developer: String,
        #[arg(long, value_parser = ["approved", "rejected"])]
        decision: String,
        #[arg(long)]
        reason: String,
        #[arg(long, value_name = "ABSENT_FILE")]
        output: PathBuf,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Write one recorded Task output to an absent file (Markdown for a Document, or JSON)
    #[command(
        after_long_help = "Formats:\n  json            the recorded artifact envelope, for machines and audit\n  markdown        the canonical rendering the checks and the reviewer judged\n  human-markdown  a readable presentation of a Document or report: its sections, sources,\n                  plain-language result and matching review; derived, not itself reviewed\n\nExamples:\n  af task output TASK_ID --port report --format human-markdown --output report.md\n  af task output TASK_ID --port report --format markdown --output report-canonical.md"
    )]
    Output {
        task_id: String,
        #[arg(long)]
        port: String,
        #[arg(long, default_value="json", value_parser=["json","markdown","human-markdown"])]
        format: String,
        #[arg(long)]
        output: PathBuf,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Record an exact outer-plan or pending experiment approval signed by a captured developer key
    Approve {
        task_id: String,
        #[arg(long)]
        payload: PathBuf,
        #[arg(long)]
        signature: PathBuf,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Record an exact plan rejection signed by a captured developer key
    Reject {
        task_id: String,
        #[arg(long)]
        payload: PathBuf,
        #[arg(long)]
        signature: PathBuf,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Run or resume a captured Task using its exact recorded plan and authority
    Run {
        task_id: String,
        /// Confirm the exact plan shown by plan/start/explain; stale identities refuse
        #[arg(long, value_name = "PLAN_ID", conflicts_with = "execute")]
        confirm_plan: Option<String>,
        /// Explicit automation opt-in: run the current plan without preview confirmation
        #[arg(long)]
        execute: bool,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Explain a captured Task's ports, hierarchy, bindings, coverage and budgets
    Explain {
        task_id: String,
        /// Render the expanded Pipeline hierarchy as an ASCII tree
        #[arg(long, conflicts_with = "json")]
        tree: bool,
        /// Inspect an exact recorded plan, including an earlier revision's plan
        #[arg(long, value_name = "PLAN_ID")]
        plan: Option<String>,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Export reusable definitions and contract fixtures into an absent directory
    Export {
        task_id: String,
        #[arg(long)]
        name: String,
        /// Absent directory, as a safe path relative to --repo (an absolute path is refused)
        #[arg(long)]
        destination: String,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Deliver a verified Task to a new local branch and worktree
    #[command(
        long_about = "Deliver a verified Task's derived Snapshot to a new local branch and \
worktree.\n\nThe target must be a clean repository at the Task's recorded source Snapshot; the \
branch and the worktree path must both be absent. Durable, idempotent, locally verified before \
it is reported complete.\n\nNever: mutates the current checkout, commits on your behalf, pushes, \
opens a pull request, or contacts a remote.",
        after_long_help = "Examples:\n  af task deliver TASK_ID --repo . --branch af/TASK_ID --worktree ../TASK_ID --confirm TASK_ID --json"
    )]
    Deliver {
        /// Task id
        task_id: String,
        /// Repository to deliver into
        #[arg(long, value_name = "DIR", default_value = ".", help_heading = "Target")]
        repo: PathBuf,
        /// New branch name (must not exist)
        #[arg(long, value_name = "NAME", help_heading = "Target")]
        branch: String,
        /// New worktree path (must not exist)
        #[arg(long, value_name = "DIR", help_heading = "Target")]
        worktree: PathBuf,
        /// Repeat the Task id to confirm
        #[arg(long, value_name = "TASK_ID", help_heading = "Target")]
        confirm: String,
        /// Explicit state directory
        #[arg(long, value_name = "DIR", help_heading = "Selector")]
        state: Option<PathBuf>,
        /// The Workspace whose Store to use, by label (`af project workspaces` lists them)
        #[arg(
            long,
            value_name = "LABEL",
            conflicts_with = "state",
            help_heading = "Selector"
        )]
        workspace: Option<String>,
        /// One JSON document on stdout instead of text
        #[arg(long, help_heading = "Output")]
        json: bool,
    },
    /// Record whether a later commit exactly adopts a delivered light optimization
    ObserveAdoption {
        /// Optimization Task id whose local delivery is being observed
        task_id: String,
        /// Commit or ref to compare with the delivered configuration
        #[arg(long, default_value = "HEAD")]
        commit: String,
        /// Comparable workload identity or version label
        #[arg(long)]
        workload: String,
        /// Model or Worker configuration identity or version label
        #[arg(long)]
        model: String,
        /// Execution environment identity or version label
        #[arg(long)]
        environment: String,
        /// Finished later common Task whose immutable receipts are linked as observational evidence
        #[arg(long, value_name = "TASK_ID")]
        evidence_task: Option<String>,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// List Tasks with outcome and spend
    List {
        /// Also print each Task's CAS bytes: those only it reaches and those it shares
        #[arg(long)]
        sizes: bool,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Collect finished Tasks: preview what would go; collect it only with --apply
    #[command(
        long_about = "Preview, and with --apply collect, the finished Tasks beyond the newest \
--keep whose last event is at least --older-than days old.\n\nA Task that is running, holds a \
writer lease, or is named by another Task's input bindings is never collected, and the preview \
says why. Collection appends one tombstone per Task, which keeps its ID, kind, revision, outcome, \
spend and times, and then removes every CAS object no remaining Task or Campaign record reaches. \
A collected Task is listed and shown as `collected`; its outputs cannot be written or delivered. \
Without --apply nothing is written. --apply is refused while any Task's writer lease is live.",
        after_long_help = "Examples:\n  af task gc --older-than 14 --keep 5\n  af task gc --older-than 14 --keep 5 --apply\n  af task gc --older-than 0 --keep 10 --json"
    )]
    Gc {
        /// Tasks whose last event is at least this many days old
        #[arg(long, value_name = "DAYS")]
        older_than: u64,
        /// Never collect the newest N finished Tasks
        #[arg(long, value_name = "N")]
        keep: usize,
        /// Collect the previewed Tasks (default: preview only)
        #[arg(long)]
        apply: bool,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Show one Task: identity, Snapshot, Gates, evaluation, spend
    Show {
        /// Task id
        task_id: String,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Summarize what recorded Tasks cost and how they ran, for a pull request description
    #[command(
        long_about = "Summarize what one or more recorded Tasks of one Store cost and how they \
ran: first one line per pipeline they ran, its steps in dependency order with each step's \
Worker and the gate's checks; then one row per Task, each a round, with its outcome, review \
findings by severity, tokens and active time, and a Total row; then per round its runs, wall \
time, failed Attempts and per node the Worker, Attempts, tokens, elapsed time and check \
results.\n\nThe default output is one Markdown block, ready to paste into a pull request \
description, between these two lines:\n\n  <!-- af-task-report:v1 -->\n  \
<!-- /af-task-report -->\n\n--json prints one `af/task-report@1` document instead. Tasks \
appear in the order given, numbered as rounds. Active time is the sum of the Task's runs, the \
writer leases that started an Attempt, so waiting between runs is not counted; wall time runs \
from its first to its last recorded event. A figure the Store does not record is shown as \
unknown.\n\nNever: dispatches a \
Worker, contacts a Provider, writes the Store or changes a Task. Providers appear only as kind \
and model; no Provider label, path, credential, prompt or Worker output is printed.",
        after_long_help = "Examples:\n  af task report pagination-cli\n  af task report implement-x verify-x --json"
    )]
    Report {
        /// Task ids, reported in this order
        #[arg(value_name = "TASK_ID", required = true)]
        task_ids: Vec<String>,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
}

// ------------------------------------------------------------------------------------------
// project

/// `af project survey --section NAME…`: a text-only option, so it conflicts with `--json`
/// (usage error 2). It is an argument of the subcommand rather than a field of
/// [`ProjectCommand::Survey`], whose shape every reader of `af project` matches;
/// [`survey_sections`] reads it.
fn survey_section_arg() -> clap::Arg {
    clap::Arg::new("section")
        .long("section")
        .value_name("NAME")
        .help("Only these sections of the text summary, by name; repeat or separate with commas")
        .value_parser(crate::projects::survey::SECTION_NAMES)
        .value_delimiter(',')
        .action(clap::ArgAction::Append)
        .conflicts_with("json")
}

/// The `--section` names `argv`, an `af project survey` command line, gave, in the order given;
/// empty for every other command.
pub(crate) fn survey_sections(argv: &[String]) -> Vec<String> {
    use clap::CommandFactory as _;
    Af::command()
        .try_get_matches_from(argv)
        .ok()
        .and_then(|matches| {
            let project = matches.subcommand_matches("project")?;
            let survey = project.subcommand_matches("survey")?;
            Some(
                survey
                    .get_many::<String>("section")?
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default()
}

/// `af project`: the commands of [`ProjectCommand`], then `brief`, which reads the Project
/// Store and records nothing (ADR-0160).
#[derive(Debug, Subcommand)]
pub(crate) enum ProjectCli {
    #[command(flatten)]
    Core(ProjectCommand),
    /// Print the exact Project Brief a `project` port would carry, recording nothing
    #[command(
        long_about = "Render one Project Brief variant (`af/ProjectBrief@1`) of this checkout's \
newest complete Survey, or of the complete Survey --survey names, and print exactly the \
canonical bytes a plan would bind on a `project_brief` port of that variant: what a Worker \
wired to the port reads, before any token is spent.\n\n\
A variant is a `[brief.<name>]` table of `.af/af.toml` (`sections`, `max_bytes`, `decisions`, \
`hot_paths`); `orient` (tree, vocabulary, decisions, checks, hot_paths in 8192 bytes, the newest \
12 decisions and the first 20 hot paths) and `none` are built in. Whole sections are dropped, \
last-declared first, until the record fits `max_bytes`, each named in `dropped` with its reason.\n\n\
--json prints `af/project-brief-preview@1`: the Brief's artifact ID, the Brief byte for byte as \
the text form prints it, and the context manifest entry a Worker would see (bytes and the \
runner's token estimate); or `outcome: none` with its reason (`variant_none`, `no_survey`), exit \
0. Without a Survey the text form exits 1 and names `af project refresh`; variant `none` prints \
nothing and says so on stderr. Reads the Project Store only: registers nothing, records nothing \
and writes nothing inside the checkout.",
        after_long_help = "Examples:\n  af project brief\n  af project brief --variant orient --json | jq .manifest\n  af project brief --survey sha256:… | wc -c"
    )]
    Brief {
        /// The variant to render [default: orient]
        #[arg(long, value_name = "NAME")]
        variant: Option<String>,
        /// Render this complete Survey instead of the newest complete one
        #[arg(long, value_name = "ID")]
        survey: Option<String>,
        /// One `af/project-brief-preview@1` document on stdout instead of the Brief's bytes
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProjectCommand {
    /// Show the Project: identity, name, Store, every Workspace and the newest Survey
    #[command(
        long_about = "Show the Project of the checkout af runs in: its ID, repository_id, the \
`[project] name` and `min_af` its `.af/af.toml` declared when it was registered, the Project \
Store, and every Workspace with its label, origin, checkout, Task Store, bindings file, Task-file \
directory and registration time. The text also shows the newest Survey's summary, or the reason \
of the newest SurveyFailed@1 when no Survey was recorded since; `af/project@1` is unchanged.\n\n\
Registers the Project and this checkout first when they are new. Reads nothing else and writes \
nothing inside the checkout.",
        after_long_help = "Examples:\n  af project show\n  af project show --json | jq .project_id"
    )]
    Show {
        /// One `af/project@1` document on stdout instead of text
        #[arg(long)]
        json: bool,
    },
    /// List the Project's Workspaces: label, origin, checkout, Store, bindings, Task files
    #[command(
        long_about = "List every registered Workspace of this checkout's Project, in label order: \
its label, origin (`adopted`, `created` or `delivered`), checkout, Task Store, bindings file and \
Task-file directory. A path this machine's `workspaces.toml` does not know is shown as unknown.",
        after_long_help = "Examples:\n  af project workspaces\n  af project workspaces --json"
    )]
    Workspaces {
        /// One `af/project-workspaces@1` document on stdout instead of text
        #[arg(long)]
        json: bool,
    },
    /// Show the newest Survey of the Project
    #[command(
        long_about = "Show the newest Survey of this checkout's Project: one kernel-measured \
description of the head (`af/ProjectSurvey@2`) with the sections identity, tree, history, \
contributors, decisions, vocabulary and checks, each complete, partial or unavailable with the \
bound it reached. Text is one line per section, and --section picks sections of the text only; \
--json prints the complete record unchanged, its canonical bytes, every number decimal text, \
and --section with --json is a usage error (exit 2).\n\nRecords nothing. Fails (exit 1) when \
the Project has no Survey yet, or when the Storage Budget collected its newest one, and says \
which; `af project refresh` records one.",
        after_long_help = "Examples:\n  af project survey\n  af project survey --section tree --section history\n  af project survey --json | jq '.history.hot_paths[].path'",
        arg = survey_section_arg()
    )]
    Survey {
        /// The `af/ProjectSurvey@2` record on stdout instead of text
        #[arg(long)]
        json: bool,
    },
    /// Measure the head into a new Survey now, unless its survey key is unchanged
    #[command(
        long_about = "Run the Surveyor now and wait: measure this checkout's head into a Survey \
and record it in the Project Store, then print its summary. A complete newest Survey with an \
unchanged survey key (the head commit, the digest of `refs/heads` and `refs/tags`, the policy \
digest and the Surveyor version) is reused: the command prints `unchanged: head …, refs …, \
policy …, surveyor …` with that Survey's summary and records nothing. A new tag or branch, a \
policy change or a partial newest Survey records a new one; --force records one regardless of \
the key.\n\nThe Surveyor reads the repository only through supervised git plumbing (ls-tree, \
cat-file, log, rev-list, for-each-ref), every stream read as it arrives, under one deadline, \
`[survey] max_wall_ms`, checked at every item; a section it cannot finish is partial or \
unavailable and the Survey is still recorded. `max_paths`, `max_entries`, `max_file_bytes` and \
`max_survey_bytes` bound the rest. It calls no Provider, runs no hook or project code and \
writes nothing inside the checkout. Contributors are named by author name or, under \
`[survey] identity = \"email_hash\"`, by the first 16 hex digits of SHA-256 of the lowercased, \
trimmed email; an author email is never stored. A Survey that cannot be taken, or whose record \
is over `max_survey_bytes`, records SurveyFailed@1, which `af project show` prints, and exits 1.",
        after_long_help = "Examples:\n  af project refresh\n  af project refresh --force\n  af project refresh --json | jq .completeness\n  AF_SURVEY__MAX_COMMITS=1000 af project refresh"
    )]
    Refresh {
        /// The recorded (or unchanged) `af/ProjectSurvey@2` record on stdout instead of text
        #[arg(long)]
        json: bool,
        /// Record a Survey even when the survey key is unchanged
        #[arg(long)]
        force: bool,
    },
    /// List the Project's Surveys, newest first
    #[command(
        long_about = "List the Survey chain of this checkout's Project, newest first: each \
Survey's ID, when it was recorded, its head, its cutoff and its completeness; a Survey the \
Storage Budget collected is answered from its SurveyCollected@1 tombstone and marked \
`collected`, and one whose record is of a superseded version (`af/ProjectSurvey@1`, another af \
release's) is listed by its ID and time as `unreadable: superseded version` and kept. --json \
prints `af/project-history@2`. Records nothing.",
        after_long_help = "Examples:\n  af project history\n  af project history --json | jq -r '.surveys[0].survey_id'"
    )]
    History {
        /// One `af/project-history@2` document on stdout instead of text
        #[arg(long)]
        json: bool,
    },
    /// Create, adopt, or remove one Workspace
    #[command(subcommand_required = true, arg_required_else_help = true)]
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkspaceCommand {
    /// Create a worktree, a branch and a Workspace directory, and register them
    #[command(
        long_about = "Create a Workspace for one line of work: a new branch (`af/LABEL` unless \
--branch) at this checkout's HEAD (unless --from), a worktree of it at `<checkout>-wt-LABEL` \
beside this checkout (unless --path), and the directory \
`$XDG_STATE_HOME/af/project/<hex>/workspaces/LABEL/` holding an empty `state/` for its Task \
Store, an empty `tasks/` for its Task files and a `bindings.toml` template that names no \
account. The Workspace is registered with origin `created`.\n\nThe worktree is filled the way \
`af task deliver` fills one: `git worktree add --no-checkout`, the index by `git read-tree`, and \
the files of the commit's tree written from a committed capture, so no checkout, filter or hook \
of the repository runs on this machine.\n\nEvery check runs before anything is made: a taken \
label, an existing path, branch or Workspace directory, and a --from commit whose root commits \
are not this Project's are refused (exit 1); a --path at or inside this checkout, inside another \
Workspace or inside `$XDG_STATE_HOME/af` is a usage error (exit 2). When a later step fails, what \
was made is undone newest first, and the message says what was undone and what remains. Never: \
commits, pushes, or contacts a remote.",
        after_long_help = "Examples:\n  af project workspace new research\n  af project workspace new research --branch agent/research --from main\n  af project workspace new spike --path ../spike"
    )]
    New {
        /// The new Workspace's label: letters, digits, `-` and `_`
        #[arg(value_parser = workspace_label)]
        label: String,
        /// The new branch [default: af/LABEL]
        #[arg(long, value_name = "NAME")]
        branch: Option<String>,
        /// Where the worktree goes, outside this checkout, every Workspace and af's state
        /// [default: <checkout>-wt-LABEL beside this checkout]
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
        /// The commit the branch starts at, one of this Project [default: this checkout's HEAD]
        #[arg(long, value_name = "REV")]
        from: Option<String>,
        /// One `af/project-workspaces@1` document on stdout instead of text
        #[arg(long)]
        json: bool,
    },
    /// Register an existing checkout of this Project as a Workspace
    #[command(
        long_about = "Register an existing checkout of this Project (a worktree or a clone) as a \
Workspace of origin `adopted`. Its Store is the default Task Store for that checkout unless \
--state names another; --bindings and --tasks name its bindings file and Task-file directory. \
This is how a hand-made directory of `tasks/`, `bindings.toml` and `state/` becomes a Workspace \
without moving a byte.\n\nA checkout of another Project is refused, naming both Project IDs; \
a checkout already registered is refused, naming its label.",
        after_long_help = "Examples:\n  af project workspace add ../afactory-wt-research --label research --state \"$W/state\" --bindings \"$W/bindings.toml\" --tasks \"$W/tasks\""
    )]
    Add {
        /// The checkout to adopt
        #[arg(value_name = "PATH")]
        path: PathBuf,
        /// Its label [default: the checkout's directory name]
        #[arg(long, value_name = "LABEL", value_parser = workspace_label)]
        label: Option<String>,
        /// Its Task Store [default: the checkout's default Task Store]
        #[arg(long, value_name = "DIR")]
        state: Option<PathBuf>,
        /// Its bindings file, which `--workspace` supplies as `--bindings`
        #[arg(long, value_name = "FILE")]
        bindings: Option<PathBuf>,
        /// The directory holding its Task files
        #[arg(long, value_name = "DIR")]
        tasks: Option<PathBuf>,
        /// One `af/project-workspaces@1` document on stdout instead of text
        #[arg(long)]
        json: bool,
    },
    /// Unregister a Workspace; keep its Store, and its worktree unless asked
    #[command(
        long_about = "Unregister a Workspace. Its Task Store is never removed: when it still \
holds a Task that is not collected, the command says so and names `af task gc`. \
--delete-worktree also removes the checkout, only a linked worktree whose files equal its HEAD \
commit, compared without running any filter or hook of the repository; the branch stays.",
        after_long_help = "Examples:\n  af project workspace remove research\n  af project workspace remove research --delete-worktree"
    )]
    Remove {
        /// The Workspace's label
        #[arg(value_parser = workspace_label)]
        label: String,
        /// Also remove the worktree, when its files equal its commit
        #[arg(long)]
        delete_worktree: bool,
        /// One `af/project-workspaces@1` document on stdout instead of text
        #[arg(long)]
        json: bool,
    },
}

/// A Workspace label: the safe-name rule of Task IDs, refused as a usage error.
fn workspace_label(value: &str) -> Result<String, String> {
    if review_core::project::is_workspace_label(value) {
        Ok(value.to_string())
    } else {
        Err("a label is 1 to 128 ASCII letters, digits, `-` or `_`, starting with a letter or digit".into())
    }
}

#[derive(Debug, Args, Clone)]
pub(crate) struct TaskInspectArgs {
    /// Repository the Task belongs to
    #[arg(
        long,
        value_name = "DIR",
        default_value = ".",
        help_heading = "Selector"
    )]
    pub(crate) repo: PathBuf,
    /// Explicit state directory
    #[arg(long, value_name = "DIR", help_heading = "Selector")]
    pub(crate) state: Option<PathBuf>,
    /// The Workspace whose Store to use, by label (`af project workspaces` lists them)
    #[arg(
        long,
        value_name = "LABEL",
        conflicts_with = "state",
        help_heading = "Selector"
    )]
    pub(crate) workspace: Option<String>,
    /// One JSON document on stdout instead of text
    #[arg(long, help_heading = "Output")]
    pub(crate) json: bool,
}

// ------------------------------------------------------------------------------------------
// config · self · completions

#[derive(Debug, Subcommand)]
pub(crate) enum StorageCommand {
    /// Preview the sweep; remove what it names only with --apply
    #[command(
        long_about = "Preview, and with --apply perform, the sweep that runs after every `af task \
run` and `af review run`.\n\nCollection first: finished Tasks beyond the newest `keep_tasks` of \
each Store, review campaigns beyond the newest `keep_campaigns`, the Surveys of each Project beyond \
the newest `keep_surveys`, and Stores this release cannot read, each idle at least `keep_days`; and the gate pull requests and branches finished Tasks \
left. Then the budget: while af holds more than `max_bytes`, the least recently used entry goes \
first. Nothing in use is ever removed — a held lock, a live writer lease, a running campaign, the \
default or a pinned version, the running binary — nor anything used within the last hour. \
Finished Tasks go through the Store's own collection, so each keeps its tombstone.",
        after_long_help = "Examples:\n  af storage prune\n  af storage prune --apply\n  af storage prune --apply --json"
    )]
    Prune {
        /// Remove what the preview names (default: preview only)
        #[arg(long)]
        apply: bool,
        /// Machine-readable af/storage-prune@4
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum TelemetryCommand {
    /// The level, the directory, its bytes, segments, runs, the index and every problem
    #[command(
        long_about = "The level, the directory, its bytes, segments, runs, the index and every problem.\n\n\
Prints the configured `[telemetry] level`; the telemetry directory and its allocated bytes, in \
total against `[telemetry] max_bytes` and per entry (each segment with its manifest, the blobs, \
the index with its WAL and shared memory, temporary files, locks, and `unrecognized`: every \
other path beneath the directory, a symbolic link included, counted once, named up to 16, never \
read, followed or removed, so the total is every allocated byte beneath it), each sealed segment \
with its \
seal's `exit` and its capture (`complete`, or why not: `recovered, drops unknown`); the segments \
by state — `live` (a writer holds its lock), `unsealed` (nobody does: a crashed writer's, \
recovered by the next `reindex`), `sealed` and `corrupt` (interior corruption, never read); the \
runs the index knows, the Attempts whose content was withheld, and stored occurrences whose blob \
file is missing (`content unavailable: missing blob`), looked for under `gc.lock` shared or \
reported `deferred` when a sweep holds it past 2 s; the index after ingesting every new segment \
byte (`fresh`, or `direct` when another command is replacing it and the segments were read \
directly, said on stderr); the distinct problems recorded; what cannot be read (a directory of \
another telemetry version such as `v0/`, never read; a segment with an invalid line before a \
valid one or a line over the 16 KiB record bound, never indexed); and every telemetry directory \
registered on this machine, with its overlap rechecked: `none`, `overlapping`, or `unknown` when \
the Store registry cannot be read (the problem is named among the problems and the exit code is \
unchanged).\n\n\
Never: recovers or removes a segment, records telemetry, reads a Store, reads around an index of \
another schema version, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or its index is of another \
schema version (`af telemetry reindex` replaces it), 2 a refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry status\n  af telemetry status --json\n  af telemetry status --dir /backup/telemetry --json | jq .bytes.total"
    )]
    Status {
        /// Machine-readable af/telemetry-status@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// Recover crashed writers' segments and rebuild the index from every segment
    #[command(
        long_about = "Recover crashed writers' segments and rebuild the index from every segment.\n\n\
Every `.jsonl` segment whose lock nobody holds is recovered to its last complete line: the \
discarded bytes are recorded in its seal (`exit = recovered`; the writer's drop counts `null`, \
unknown, unless the writer's own seal was already written, which the recovered seal then \
supersedes), spans it never ended are marked with no end, and it is compressed; an invalid line \
before a valid one renames it `.corrupt`. Then the index is replaced under `index.lock` — taken \
exclusively within 2 s, checkpointed, closed, its database, WAL and shared-memory files unlinked \
— and rebuilt from every segment, and every blob file's digest is verified with `gc.lock` held \
shared (`blob_verification = deferred`, no blob file opened, when the lock is not had within \
2 s). Every query answers the same before and after: the \
index is derived state, never evidence.\n\n\
Never: removes a sealed segment or a blob, reads a Store, or contacts a network.\n\n\
Exit codes: 0 success, 1 the directory cannot be read or another command holds the index, 2 a \
refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry reindex\n  af telemetry reindex --json\n  af telemetry reindex --dir ./copied-telemetry"
    )]
    Reindex {
        /// Machine-readable af/telemetry-reindex@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Rebuild this telemetry directory's index instead of the configured one
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// Every run recorded: executions, active, execution and elapsed time, outcome, charge
    #[command(
        long_about = "Every run recorded: executions, active, execution and elapsed time, outcome, charge.\n\n\
One row per kernel run within its Store (a Task run, a review run, a self-optimizer run, a \
measurement): its Store (`af.store_id`), run id, kind, Task or Campaign, its retained \
executions (one per process that worked on it) and first start; its **active** time (the union \
of the executions' intervals, an overlap counted once), **execution** time (their sum, \
`af.execution.duration_sum`) and **elapsed** time (first start to last end, idle gaps \
included), an interval without a recorded end ending at its segment's last record and marked \
open; the latest execution's own outcome (none while its `af.run` has not ended); the \
Attempts' exact usage counters and charge as the kernel accounted \
them; failures; coverage (`complete` when every retained segment was sealed by its writer with \
nothing dropped); and the retained history: `prefix:k` when executions 1 to k are retained with \
their seals and no ordinal between them is missing, `partial` with the missing ordinals \
otherwise. A run's history is never called complete: every total says `retained executions \
1–k` or `partial`, and names the open executions (a live or unsealed segment, or an `af.run` \
with no recorded end) beside the sealed ones: `retained executions 1–1, open: 2`; \
`totals_cover` lists every execution the totals cover, each open one marked.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or its index is of another \
schema version, 2 a wrong command line or a refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry runs\n  af telemetry runs --since 7d --kind task\n  af telemetry runs --json | jq '.runs[] | {run_id, active_us, history}'\n  af telemetry runs --dir ./copied-telemetry --json"
    )]
    Runs {
        /// Only runs active at or after this long ago (30m, 12h, 14d, 2w)
        #[arg(long, value_name = "D", help_heading = "Selector")]
        since: Option<String>,
        /// Only runs of this kind
        #[arg(long, value_name = "K", value_parser = ["task", "review", "optimize", "measure"], help_heading = "Selector")]
        kind: Option<String>,
        /// Machine-readable af/telemetry-runs@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// One run's span tree: start offset, total and self time, status and key attributes
    #[command(
        long_about = "One run's span tree: start offset, total and self time, status and key attributes.\n\n\
RUN is the kernel run id `af telemetry runs` lists. The tree runs from each execution's \
`af.command` through `af.run` to `af.authority.load`, `af.plan`, every `af.node` and its \
`af.attempt` spans, and the command's `af.sweep`; each span with its start offset from the \
run's first record, total and self time (total less its ended children's union), status (`ok`, \
`error`, `unset`, or `open` when it has no recorded end) and key attributes. `--attempt ID` \
keeps that Attempt's subtree with its ancestors, `--depth N` stops N levels below the root, and \
`--span ID` prints one span with every attribute and its events. The run's retained executions \
and history head the tree, its open executions listed beside the sealed ones. When two Stores record the same run id it lists both and asks for \
`--store ID`.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or the run, Attempt or span is \
unknown, 2 a wrong command line, a run two Stores share without `--store`, or a refused \
`[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry show RUN_ID\n  af telemetry show RUN_ID --depth 2\n  af telemetry show RUN_ID --attempt ATTEMPT_ID\n  af telemetry show RUN_ID --span SPAN_ID --json\n  af telemetry show RUN_ID --store STORE_ID"
    )]
    Show {
        /// The kernel run id
        #[arg(value_name = "RUN")]
        run: String,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        store: Option<String>,
        /// Only this Attempt's subtree, with its ancestors
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        attempt: Option<String>,
        /// Only this span, with every attribute and its events
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        span: Option<String>,
        /// Only spans at most this many levels below the root
        #[arg(long, value_name = "N", help_heading = "Selector")]
        depth: Option<u64>,
        /// Machine-readable af/telemetry-show@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// One run's configuration record in one execution: version, configuration with origins, plan, bindings, Providers, host
    #[command(
        long_about = "One run's configuration record in one execution: version, configuration with origins, plan, bindings, Providers, host.\n\n\
RUN is the kernel run id `af telemetry runs` lists. The record (`af/TelemetryConfig@2`) is \
written at `spans` and above for each run an execution works on — a measurement run inside a \
Task Attempt has its own — as `config` parts that each name the run (its run id and its \
`af.run` span), in three sections of one fixed order. When the run opens: `configuration` \
(`af --version --json`; the effective configuration, one row per key with its layer, its file \
(home-relative) and line, and its value — a value whose origin is the environment is `<env>`, \
never the value; the digests of `.af/af.toml` and `.af/af.lock`) and `machine` (the Provider \
registry entries by name and kind, grant paths as placeholders; the harness versions the \
preflight probed and the Claude Code managed-settings digests; the host; the names, never the \
values, of the `AF_*` and `OTEL_*` variables present). When the plan resolves: `plan` (the \
policy revision, the pipeline's name, digest and nodes, the Task's id, kind and pipeline, the \
execution bindings — node, Worker, Provider, model, effort). It assembles the run's parts and \
says, per section, `complete`, `incomplete` (a part is missing) or `unknown` (none was \
recorded: a run refused before its plan resolved has no plan section), the record `incomplete` \
unless every section is complete. The latest execution by default (the greatest committed \
ordinal), `--execution ID` for another. The texts the record names (the pipeline, the Task \
file, `.af/af.toml`) are listed with their digest and size. When two Stores record the same run \
id it lists both and asks for `--store ID`.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or the run or execution is \
unknown, 2 a wrong command line, a run two Stores share without `--store`, or a refused \
`[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry config RUN_ID\n  af telemetry config RUN_ID --execution EXECUTION_ID\n  af telemetry config RUN_ID --json | jq '.rows[] | select(.layer == \"environment\")'"
    )]
    Config {
        /// The kernel run id
        #[arg(value_name = "RUN")]
        run: String,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        store: Option<String>,
        /// This execution (`af.execution_id`) instead of the latest
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        execution: Option<String>,
        /// Machine-readable af/TelemetryConfig@2
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// Follow the live segments and print every span end and event as it is written
    #[command(
        long_about = "Follow the live segments and print every span end and event as it is written.\n\n\
Like `tail -f`: every live segment (or, with RUN, those that carry that run) is read from its \
start and followed; each span end (with its duration and status) and each event is printed as \
the writer writes it, within `[telemetry] flush_latency_ms` of its creation while a run is open. \
It exits when every followed segment is sealed — its sealed file published and readable, not \
merely its seal line written — or recovered; with no live segment it waits for one. With RUN, \
a run whose every execution was sealed before tail started is printed from its sealed files and \
tail exits at once. A segment \
no writer holds any more (a crashed writer's, or one whose publication was abandoned) is \
recovered as any reader able to lock it would, then read sealed. A followed segment gone \
without a sealed file or a recovery of tail's own (renamed `.corrupt` by another reader's \
recovery) is never a seal: tail names it and fails. With `--json` each line is one \
`af/telemetry-tail@1` object. With RUN the run is looked up first, within `--store` when given, \
and followed in one Store for its whole lifetime: the one `--store` names, else the one that \
records it at the start. When two Stores record the run id, at the start or once a second one \
starts it or seals a segment of it, it asks for `--store ID` before printing anything of the \
second.\n\n\
Never: reads a Store, records telemetry, removes a segment, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory or a followed segment cannot be read (a \
segment with interior corruption, or one gone without a seal, included), a `.corrupt` segment \
held RUN (or exists while RUN cannot be found: tail names it), or RUN is unknown (never said \
while a segment is unreadable: tail then names that segment), 2 a wrong command line, a run \
two Stores share without `--store`, or a refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry tail\n  af telemetry tail RUN_ID\n  af telemetry tail RUN_ID --json | jq -c 'select(.record == \"span_end\")'"
    )]
    Tail {
        /// Only this kernel run's records
        #[arg(value_name = "RUN")]
        run: Option<String>,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        store: Option<String>,
        /// One af/telemetry-tail@1 object per line
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Follow this telemetry directory instead of the configured one
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// One run's committed Store events, Finding and Demand summaries and accounting, in order
    #[command(
        long_about = "One run's committed Store events, Finding and Demand summaries and accounting, in order.\n\n\
RUN is the kernel run id `af telemetry runs` lists. Telemetry mirrors every event the kernel \
commits to a Store after its commit (`af.store.event`: type, sequence, id, causation, artifact \
references, and the payload's digest and size), records a Finding summary for every Report \
admitted into the Ledger and whenever a summarized field changes (its status, severity, \
Report artifact, ...), with the admitting Worker's bound model when the kernel recorded it, and \
a Demand summary keyed by the Demand id, naming the Demand's own Subject and that Subject's \
Snapshot, with its lifecycle (opened, evidence satisfied with the Evidence id — an admitted \
reuse that satisfies a stale Demand again included —, stale, waived); every summary and \
lifecycle step carries the committed sequence of the event that caused it. This prints them \
from the telemetry directory alone, each Store log in its committed sequence (never in the \
order they were recorded; at one sequence: the event, then Finding summaries, dispositions, \
satisfactions, Demand summaries and other Demand steps), the latest summary being the one of \
the greatest sequence. \
Beside them, each Attempt's charge: the kernel's accounting after its last committed \
accounting transition (`af.usage.state` with the highest `af.usage.sequence`), and, never \
merged with it, the latest sidecar observation of each execution that wrote one, the order \
across executions `unknown`; the run's totals are sums of the Attempts' charges. Coverage is \
`complete` only when every committed event of every Store log mirrored, from its first \
sequence to the last mirrored, and every summary reached Telemetry with nothing dropped, every \
retained execution sealed and none missing or open; otherwise `incomplete` with every reason. \
The history marker says which executions everything covers. When two Stores record the same \
run id it lists both and asks for `--store ID`.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or the run is unknown, 2 a \
wrong command line, a run two Stores share without `--store`, or a refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry ledger RUN_ID\n  af telemetry ledger RUN_ID --json | jq '.findings[] | {key, status}'\n  af telemetry ledger RUN_ID --json | jq .accounting.totals\n  af telemetry ledger RUN_ID --dir ./copied-telemetry"
    )]
    Ledger {
        /// The kernel run id
        #[arg(value_name = "RUN")]
        run: String,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        store: Option<String>,
        /// Machine-readable af/telemetry-ledger@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// What the Storage Budget's sweep would evict from the telemetry kind; --apply evicts it
    #[command(
        long_about = "What the Storage Budget's sweep would evict from the telemetry kind; --apply evicts it.\n\n\
Every registered telemetry directory, whatever the current level or `dir`, is listed as \
`af storage` lists the kind `telemetry`: one entry per sealed or corrupt segment whose bytes are \
what its eviction frees (the segment, its manifest and the blobs no other segment references), \
live and unsealed segments in use, another telemetry version's directory, each directory's \
`shared` remainder (the index, locks, shared blobs, temporaries), and a directory overlapping \
other accounted state reported `overlapping`, counted once and never evicted. The step is \
ADR-0144's: least recently used first (corrupt first among equals) while af is over \
`[storage] max_bytes` or telemetry over `[telemetry] max_bytes`, by age past `[storage] \
keep_days` too, never an entry in use or used within the last hour. With `--apply` it evicts — \
each entry checked again just before it is unlinked, and one that changed since the inventory \
(replaced, locked, used within the hour) left in place and listed under `changed` — then removes temporary files older than an hour, blobs no remaining segment references (under \
`gc.lock`, skipped when a reader holds it past 2 s) and the index's free pages (`VACUUM`, or the \
index deleted for the next reader to rebuild when there is no room), measured as allocated \
bytes; registered directories that no longer exist are dropped from the registry. Without \
`--apply` nothing is removed.\n\n\
Never: removes anything of another kind, reads a span, records telemetry, or contacts a \
network.\n\n\
Exit codes: 0 success, 1 the storage or telemetry configuration cannot be read, 2 a wrong \
command line or a refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry gc\n  af telemetry gc --apply\n  af telemetry gc --json | jq '.removals'"
    )]
    Gc {
        /// Evict and reclaim; without it, only say what would be removed
        #[arg(long)]
        apply: bool,
        /// Machine-readable af/telemetry-gc@1
        #[arg(long, help_heading = "Output")]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Print the effective configuration
    Show {
        /// Annotate every value with the file and line it came from
        #[arg(long)]
        origin: bool,
        /// One JSON document on stdout
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Repository whose project layers to include (default: the current directory)
        #[arg(long, value_name = "DIR", help_heading = "Selector")]
        repo: Option<PathBuf>,
    },
    /// Open one layer's file in $EDITOR (creating it if absent)
    Edit {
        /// Which layer to edit
        #[arg(long, value_enum, default_value_t = LayerArg::Project)]
        layer: LayerArg,
        /// Repository for the project and local layers (default: the current directory)
        #[arg(long, value_name = "DIR", help_heading = "Selector")]
        repo: Option<PathBuf>,
    },
    /// Print the files every layer would read, existing or not
    Paths {
        /// Repository whose project layers to include (default: the current directory)
        #[arg(long, value_name = "DIR", help_heading = "Selector")]
        repo: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum LayerArg {
    User,
    Directory,
    Project,
    Local,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SelfCommand {
    /// Capture project history and preview economics or an explicitly installed experiment
    #[command(
        long_about = "Capture declared, project-associated history and preview an Optimization Task. With a captured .af/optimization-policy.json, --strategy light runs the installed diagnose/propose/experiment path; otherwise the command retains token-free report-only analysis. Every generated experimental closure requires a separate signed decision. Raw transcripts, protected case bodies, private reasoning and undeclared paths are never sent to diagnosis/proposal Workers. The command is dispatched through the project's pinned af release.",
        after_long_help = "Examples:\n  af self optimize --strategy light\n  af self optimize --since 30d\n  af self optimize --all-history --execute\n  af self optimize --experiment --execute\n  af task output TASK_ID --port report --format markdown --output optimization.md"
    )]
    Optimize {
        /// Include observations at or after this duration before the captured UTC cutoff
        #[arg(long, value_name = "DURATION", conflicts_with = "all_history")]
        since: Option<String>,
        /// Read all declared history subject to captured bounds
        #[arg(long, conflicts_with = "since")]
        all_history: bool,
        /// Versioned strategy declaration; light uses an installed policy when present
        #[arg(long, default_value = "light", value_parser = ["light", "heavy"])]
        strategy: String,
        /// Declared source configuration; paths are never discovered implicitly
        #[arg(
            long,
            value_name = "FILE",
            default_value = ".af/optimization-sources.toml"
        )]
        history_config: PathBuf,
        /// Explicitly execute the fixed report-only plan after capture
        #[arg(long)]
        execute: bool,
        /// Request the installed bounded baseline/candidate experiment profile
        #[arg(long)]
        experiment: bool,
        /// Concrete bounded configuration edits; runs the controlled candidate Pipeline
        #[arg(long, value_name = "FILE")]
        candidate: Option<PathBuf>,
        /// Repository whose pin, source identity and catalog apply
        #[arg(long, value_name = "DIR", default_value = ".")]
        repo: PathBuf,
        /// Explicit common Task state directory outside the repository
        #[arg(long, value_name = "DIR")]
        state: Option<PathBuf>,
        /// One JSON document on stdout
        #[arg(long)]
        json: bool,
    },
    /// What is installed, the default, its receipt, and the pin that applies here
    Status {
        /// One JSON document on stdout
        #[arg(long)]
        json: bool,
    },
    /// Install a newer release as the default (or just check for one)
    #[command(
        long_about = "Install a newer release as the default.\n\nDownloads the release for this \
machine's target, verifies it against the release's SHA256SUMS (signed with the release key; \
the signature is required), installs it beside the other versions, and retargets the default \
symlink atomically. The previous version stays installed for `af self rollback`.\n\n\
Never: changes what a pinned project runs, activates a release older than 0.8.0, or stores a \
token.",
        after_long_help = "Examples:\n  af self update --check        exit 10 when a newer release exists\n  af self update\n  af self update --version 0.9.0"
    )]
    Update {
        /// Only report whether a newer release exists (exit 10 when it does)
        #[arg(long)]
        check: bool,
        /// Install this exact version instead of the newest
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// Consider pre-releases
        #[arg(long)]
        rc: bool,
    },
    /// Make the previously active version the default again
    Rollback,
    /// Install an exact version, making it the default when no managed default exists
    Install {
        /// Version to install (e.g. 0.8.0)
        version: String,
    },
    /// Remove an installed version (never the default, never a pinned one)
    Remove {
        /// Version to remove
        #[arg(add = ArgValueCompleter::new(complete_version))]
        version: String,
    },
    /// Remove installed versions beyond `[self] keep_versions`, keeping pins and the default
    Prune,
    /// Install completions and man pages for your shell
    #[command(
        long_about = "Install completions and man pages for your shell.\n\nWithout --write, prints \
what would be written and the one line to add to your rc file when the shell needs it. With \
--write, writes only into the shell's autoload directory; rc files are never edited.",
        after_long_help = "Examples:\n  af self setup-shell\n  af self setup-shell --shell zsh --write"
    )]
    SetupShell {
        /// Shell to set up (default: $SHELL)
        #[arg(long, value_enum)]
        shell: Option<Shell>,
        /// Write the files instead of printing the plan
        #[arg(long)]
        write: bool,
    },
    /// Remove every version this receipt installed, and the default symlink
    Uninstall {
        /// Also remove af's config, state, cache, and data directories, every registered Store,
        /// and the Claude history af's Workers left in each registered Claude config directory
        #[arg(long)]
        purge: bool,
    },
    /// Render man pages into a directory
    #[command(hide = true)]
    Man {
        /// Output directory (man1 pages are written directly into it)
        out_dir: PathBuf,
    },
    /// Refresh the cached release check (internal; spawned detached)
    #[command(hide = true)]
    RefreshCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

impl Shell {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Elvish => "elvish",
            Self::Powershell => "powershell",
        }
    }
}
