//! The `af` command tree: one clap definition feeds parsing, `--help`, `af help <topic>`, shell
//! completions, and the man pages, so none of them can drift from the others.
//!
//! Help follows three rules. `af` alone shows the namespaces (on a terminal it opens the browser
//! instead, see `tui`); `af <namespace>` shows only that namespace; `af <command> --help` says
//! what the command does, what it never does, its options grouped by role, and examples.
//!
//! Every example runs as written (`tests/it/help_examples.rs` runs each one in an onboarded
//! repository with a private home): one command per line, no wider than the help page, a long
//! one continued with ` \`, and an optional shell comment at its end or on the line above it.
//! `# needs: …` names a precondition the reader must supply first (a Task, a Campaign, a
//! registered Provider, the network), and `# exit N` the exit code the example documents when it
//! is not 0; any other comment only describes the line.

use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use clap_complete::engine::ArgValueCompleter;

use crate::selfmgmt::{complete_campaign, complete_version};

pub(crate) const REVIEW_PIPELINE: &str = ".af/pipelines/review.toml";

/// `af review --help`: its eighteen subcommands grouped by where a Campaign is in its life, in
/// place of clap's flat list. The run options the namespace also accepts are `af review run`'s,
/// documented there. A unit test holds every line to the subcommand's own name and summary.
const REVIEW_HELP_TEMPLATE: &str = "\
{before-help}{about-with-newline}
{usage-heading} {usage}

Plan, token-free and without Campaign state:
  plan                  Show what a review would run, token-free, without Campaign state
  render                Compose one Worker's exact input, token-free, without Campaign state

Run:
  run                   Execute the pipeline and fold results into the ledger

Inspect a Campaign:
  ledger                List the ledger: findings, dispositions, and open demands
  show                  Show one ledger entry by key
  report                Render the Campaign report
  export                Export a verified patch proposal, by id or by the finding it fixes

Work its findings and demands:
  resolve               Resolve a finding without a fix: rejected, or tracked as wontfix
  attest-change         Attest that a region changed in response to a finding
  verify-fix            Record a verification of an attested fix
  challenge-resolution  Challenge a non-fixed resolution
  group                 Group one finding under another
  ungroup               Undo a grouping
  evidence              Attach or apply evidence to a demand
  demand                Waive a demand
  policy-time           Advance the Campaign's policy clock

Campaigns and maintenance:
  campaigns             List Campaigns under the state root
  gc                    Reclaim Campaign state: list what would go; remove it only with --apply

Run options (`af review [OPTIONS]` is `af review run [OPTIONS]`): see `af review run --help`.{after-help}";

const AF_ABOUT: &str =
    "Afactory — model Workers review and change a git repository in sandboxes, on a budget";
/// `af --help`: what af is in one sentence, the four first steps, and where the words and the
/// topics are explained. The one command list is clap's, in the order a first run needs it.
pub(crate) const AF_LONG_ABOUT: &str = "\
af runs model and command Workers in sandboxes over an immutable Snapshot of a git repository, \
on a token budget, and never publishes: `af review` folds what reviewers find into a ledger, and \
`af task` turns a Task file into a verified change you deliver to a new local worktree.

Start here (`af help start` explains each step):
  1. af provider detect --apply   find the claude/codex logins on this machine and register them
  2. af onboard                   in a repository: preview `.af/` (token-free); --apply writes it
  3. git add .af && git commit    review that authority, then commit it
  4. af review plan --policy-rev HEAD --base HEAD --uncommitted
                                  token-free; pass --provider NODE=ID for each model Worker
  af doctor                       check this machine and repository, token-free

Task, Worker, Attempt, Snapshot, Provider and the other words are explained in \
`af help concepts`; `af help` lists every topic (config, layers, environment, exit-codes, json, errors, \
providers, task-states, task-file, agents, self, storage, telemetry). `af` alone at a terminal \
opens a read-first browser over this scope's settings, Providers, Workers, pipelines, Tasks and \
review Campaigns: `?` there lists its keys, and `:` runs any af command.";

const AF_AFTER_HELP: &str = "\
Exit codes (`af help exit-codes`):
  0 pass · 1 error · 2 usage · 3 fail · 4 incomplete · 10 outdated (self update --check)
  af task run: 5 gate failed · 6 provider failed · 7 expired (`af help exit-codes`)

Examples:
  # preview `.af/` for this repository, token-free\n  af onboard
  af onboard --apply             # needs: a repository without `.af/`
  # needs: one registered Provider for every model Worker
  af review plan --policy-rev HEAD --base HEAD --uncommitted
  # needs: Providers claude-main and codex-main (af provider setup)
  af review --policy-rev HEAD --base HEAD --uncommitted \\
    --provider correctness=claude-main --provider architecture=codex-main
  af self status                 # what is installed and which pin applies here
  # what is ready on this machine and in this repository\n  af doctor

Review and commit generated `.af/` before selecting it with --policy-rev. These review examples
use trusted policy at HEAD and compare working-tree changes against HEAD; run refuses an empty diff.
To execute, also supply --provider NODE=ID for every model Worker required by that policy.";

/// What bare `af` prints first when stdout is not a terminal, before its usage on stderr.
pub(crate) const FIRST_RUN_HINTS: &str = "\
af: no command given, and the browser opens only when stdout is a terminal
first run? af provider detect · af onboard · af help start";

#[derive(Debug, Parser)]
#[command(
    name = "af",
    bin_name = "af",
    about = AF_ABOUT,
    long_about = AF_LONG_ABOUT,
    after_long_help = AF_AFTER_HELP,
    disable_help_subcommand = true,
    disable_version_flag = true,
    subcommand_help_heading = "Commands, in the order a first run needs them",
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
        display_order = 9,
        long_about = "Capture shared Pipeline, Worker and Task-kind packages from an exact Git revision.\n\n\
`init` writes starter packages and `sync` writes a captured import into --destination. The \
destination is a safe project-relative path under --repo (default `.`) that must not exist yet: \
an absolute path, a backslash, an empty, `.`, `..` or `.git` segment, or a path through a \
symlink or a non-directory is refused, and an existing destination is never overwritten. \
`init --profile` names a starter set, not an installed Task-kind profile.",
        after_long_help = "Examples:\n  af catalog init --profile software --destination task-demo\n  # needs: a Git catalog checkout at ../team-pipelines\n  af catalog sync --source ../team-pipelines --revision main \\\n    --manifest catalog.toml --destination catalog-copy --json\n  af catalog test --source . --json   # needs: a committed catalog.toml"
    )]
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    /// Plan, run, and work a review Campaign
    #[command(
        display_order = 3,
        long_about = "Plan, run, and work a review Campaign.\n\n\
A Campaign reviews one Subject (a diff or the working tree) against committed `.af/` policy. \
`plan` is token-free. `run` executes the pipeline inside sandboxes and folds results into the \
ledger; the remaining commands read or resolve that ledger. Flags given directly to `af review` \
are shorthand for `af review run`.",
        after_long_help = "HEAD must contain trusted, committed `.af/`; make candidate changes first. To \
execute, also supply --provider NODE=ID for every model Worker the policy names; `af review plan` \
lists them.\n\nExamples:\n  # needs: one registered Provider for every model Worker\n  \
af review plan --policy-rev HEAD --base HEAD --uncommitted --json\n  \
# needs: Providers claude-main and codex-main (af provider setup)\n  \
af review --policy-rev HEAD --base HEAD --uncommitted \\\n    \
--provider correctness=claude-main --provider architecture=codex-main\n  \
# needs: Campaign pr-42 (af review run --campaign pr-42)\n  af review ledger --campaign pr-42\n  \
af review report --campaign pr-42 --format md   # needs: Campaign pr-42",
        override_usage = "af review <COMMAND>\n       af review [--heavy] [RUN OPTIONS]   (shorthand for `af review run`)",
        help_template = REVIEW_HELP_TEMPLATE,
        args_conflicts_with_subcommands = true,
        subcommand_negates_reqs = true,
        arg_required_else_help = true
    )]
    Review(ReviewNamespace),
    /// Inspect and preflight the configured model providers
    #[command(
        display_order = 1,
        alias = "providers",
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Inspect and preflight the configured model providers.\n\n\
Providers are named in the user configuration (`~/.config/af/providers.toml`); `detect` finds \
the official CLIs and their logins on this machine and registers them with --apply; `status` reads \
that registry and the machine's harness logins without contacting any model. `models` lists \
each Provider's model catalog and effort levels, token-free, from the harness's own protocol. \
`doctor` runs a bounded, charged preflight for the bindings you name.\n\n\
Results go to stdout (add --json to `detect`, `status`, `models` and `setup` for versioned \
documents); \
progress, warnings and diagnostics go to stderr. Exit codes across this namespace: 0 success, 1 \
unclassified failure, 2 usage error, 3 human action required, 4 Provider CLI missing or \
cannot start, 5 registry conflict, 6 authentication failed, 7 optional usage or a model catalog \
unavailable.",
        after_long_help = "Examples:\n  af provider detect --apply\n  af provider status\n  af provider models --resolve 'Fable 5.1/xhigh'\n  \
af provider setup codex-main --kind codex   # needs: the codex CLI, logged in\n  \
# needs: Campaign pr-42 and Provider claude-main\n  \
af provider doctor --campaign pr-42 --provider correctness=claude-main\n\n\
See `af help providers` for the registry model and the order of first use."
    )]
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Generate or validate `.af/` review authority for a repository
    #[command(
        display_order = 2,
        long_about = "Set up a repository for af review: write the `.af/` policy a review runs \
under, then commit it. af reads review policy only from a commit, so nothing you have not \
reviewed and committed ever governs a review. Four steps:\n\n  \
1. af onboard          preview what would be written; token-free, writes nothing\n  \
2. af onboard --apply  write `.af/`, only where none exists; it never overwrites one\n  \
3. git add .af && git commit -m \"af review policy\"\n  \
4. af review plan --policy-rev HEAD --base HEAD --uncommitted   see what a review would run\n\n\
In a repository that already has `.af/`, `af onboard` checks it instead: the selected pipeline, \
its exact pins, the Worker packages, the graph and the Gates. --refresh-lock: explicitly re-pin \
the selected pipeline, every pipeline a route or the oversized policy names, and the Worker \
packages they reference; it drops a pin whose `.af/workers/NAME` package or \
`.af/pipelines/NAME.toml` file no longer exists.\n\n\
The command never calls a model, runs a Gate, reads credentials, creates Campaign state, fetches \
a pull request, commits, pushes, comments, or overwrites an existing `.af/` directory. Notes for \
agents and automation close this page.",
        after_long_help = "Runner profiles:\n  mixed   correctness = Claude Opus 5.5/high; architecture = GPT-6 Sol/high\n  claude  both Workers = Claude Opus 5.5/high\n  codex   both Workers = GPT-6 Sol/high\nWithout --runner, onboarding picks claude or codex when only that Provider kind is registered on this machine (`af provider status`), and mixed otherwise; the `profile` line says which and why.\n\nThe preview's `providers` block names, for every model Worker, a registered Provider of its kind and the --provider binding to pass, or the command that registers one; it reads the registry without starting a Provider CLI unless --probe asks.\n\nGate discovery prefers `make check`, then `scripts/verify.sh`, Rust, Go, or a package-manager test script. If none is unambiguous, pass a trusted literal.\n\nExamples:\n  \
af onboard\n  \
# needs: a repository without .af/\n  af onboard --gate 'check=make check' --apply\n  \
af onboard --runner mixed --apply   # needs: a repository without .af/\n  \
af onboard --refresh-lock\n  \
# needs: release 0.9.0; moves the pin to it\n  af onboard --refresh-lock --af 0.9.0\n\n\
Notes for agents and automation:\n  \
* `.af/af.lock` records the af release that wrote it and the archive digest of that release for \
every target. Inside such a project any `af` on PATH runs that release, installing it on demand \
only as `[self] install_pins` allows and only when the bytes match the lock and the signed \
release checksums; --refresh-lock re-pins to the running release, and --af VERSION runs the \
command under another installed-or-installable release, which is how a pin moves forward. A \
build without an install receipt (a source build) pins nothing.\n  \
* Trusting configured Worker authority and intentionally running `af review run` or `af task \
start` authorizes delivery of each Worker's declared inputs for all Attempts and later Rounds or \
stages of that Campaign or Task. Afactory does not ask for per-call confirmation.\n  \
* Review Campaigns are light by default: one closed Round, then fix concrete Findings and run \
the deterministic project gate. Do not start another Campaign. Use `--heavy` only when a human \
explicitly requests convergence review, and repeat that explicit mode when resuming it."
    )]
    Onboard(OnboardArgs),
    /// Run, follow, inspect, and deliver Tasks
    #[command(
        display_order = 4,
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Run, follow, inspect, and deliver Tasks.\n\n\
A Task file (`--file`) names the Task's ID, kind, goal and limits. The Pipeline and Worker \
packages it runs are pinned in the committed `.af/task-catalog.toml`; `af catalog init --profile software \
--destination DIR` creates a new starter directory with a working catalog and runnable Task \
files. The Task runs over a captured source Snapshot and ends at a verified or unverified \
derived Snapshot. Nothing is written back to the repository unless you `deliver` it, to a new \
local branch and worktree, after confirming the Task ID, or `materialize` it into a worktree \
you name.\n\nAn agent's loop — plan, run or run in the background, read the state, the \
findings and the diff, run another round, deliver, describe the pull request — is `af help \
agents`.",
        after_long_help = "Examples:\n  \
# needs: a Task file (af catalog init writes starters)\n  af task plan --file ../ticket.json\n  \
# needs: a planned Task and the plan ID it printed\n  af task run TASK_ID --confirm-plan PLAN_ID\n  \
af task list --json\n  \
af task show TASK_ID   # needs: a Task\n  \
# needs: a verified Task\n  \
af task deliver TASK_ID --repo . --branch af/TASK_ID --worktree ../TASK_ID \\\n    --confirm TASK_ID"
    )]
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Show this repository's Project, measure it into Surveys, and manage its Workspaces
    #[command(
        display_order = 8,
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
names the next action; `af/error@2` under --json), 2 usage error.",
        after_long_help = "Examples:\n  \
af project show\n  \
af project refresh\n  \
af project survey --json | jq .tree.files\n  \
af project history\n  \
af project brief --variant orient\n  \
af project workspaces --json\n  \
af project workspace new triage --branch agent/triage\n  \
af project workspace remove triage --delete-worktree\n  \
# needs: a worktree of this repository at ../afactory-wt-research, and $W\n  \
af project workspace add ../afactory-wt-research --label research \\\n    --state \"$W/state\"\n  \
af project workspace remove research   # needs: Workspace research\n  \
# needs: Workspace research, a Task file\n  af task plan --workspace research --file ticket.json"
    )]
    Project {
        #[command(subcommand)]
        command: ProjectCli,
    },
    /// Show the effective configuration and where each value came from
    #[command(
        display_order = 10,
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Show the effective configuration and where each value came from.\n\n\
Configuration is TOML, merged lowest to highest: built-in defaults, `/etc/af`, the user \
directory, every `.af/af.toml` in a directory above the repository, the project's `.af/af.toml`, \
`.af/af.local.toml`, then `AF_<TABLE>__<KEY>` environment overrides. Tables deep-merge, scalars \
last-wins, arrays replace. See `af help layers`.",
        after_long_help = "Examples:\n  af config show\n  af config show --origin\n  af config show --json | jq .self\n  \
af config edit --layer user   # needs: $EDITOR and a terminal"
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// See, install, update, roll back, and remove af itself, or analyze project economics
    #[command(
        display_order = 5,
        name = "self",
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "See, install, update, roll back, and remove af itself, or analyze project economics.\n\n\
Start with `af self status`: what is installed, which version is the default, and which pin \
applies here. `af self update` installs a newer release as the default.\n\n\
Installed versions live under `$XDG_DATA_HOME/af/versions/<v>/` with a receipt each; the \
default is the `~/.local/bin/af` symlink. A project whose `.af/af.lock` pins another version \
is run by that version: `af` execs it. One not installed yet is installed on demand as \
`[self] install_pins` says (\"ask\" by default: a confirmation at the terminal, none without \
one), and only when the archive matches both the digest the lock records and the release's \
signed `SHA256SUMS`, which verify every install. Updates are checked by a detached, rate-limited child and applied by the policy \
in `[self]` (see `af help self`).\n\n\
An explicit `af self install V` run from a cargo/source-installed af at the default path replaces \
that running executable with the verified release and adopts the self-managed layout. Other \
binary-management commands never touch an unmanaged binary, and install never replaces an \
unrelated file at that path. It never stores a token, changes the version a pinned project runs, \
or activates a release older than 0.8.0 (the oldest supported release).\n\n\
`self optimize` is project-scoped and stands apart: it dispatches through the project pin, \
captures only declared history, and previews a report-only common Task.",
        after_long_help = "Examples:\n  \
af self status\n  \
af self update --check   # exits 10 when a newer release exists\n  \
af self update   # needs: the network and an af installed by af self\n  \
af self rollback   # needs: an earlier version af self activated\n  \
af self setup-shell --write"
    )]
    SelfCmd {
        #[command(subcommand)]
        command: SelfCommand,
    },
    /// Show what af keeps on this machine against its storage budget, and reclaim it
    #[command(
        display_order = 11,
        args_conflicts_with_subcommands = true,
        long_about = "Show what af keeps on this machine against its storage budget, and reclaim it.\n\n\
af keeps at most `[storage] max_bytes` (\"auto\" by default: 20 GiB, or 40 GiB on a host whose cache \
volume has more than 100 GiB free) between runs: warm build keys, warm \
Workspaces, review campaign Stores, Task Stores (in the state root and registered elsewhere), \
Project Stores (counted, never evicted), each Survey a Project Store holds (collected one at a time behind a tombstone, never the newest complete one) and installed versions. `af storage` prints, per kind, the entries, their bytes and their oldest and \
newest use; the Stores this release cannot read, with the reason; the total against the budget; \
and the free bytes against `[storage] min_free_bytes`, the floor below which af refuses to start a \
check or a Worker Attempt. It reads only; `af storage prune` previews the sweep that runs after \
every `af task run` and `af review run`, and `af storage prune --apply` performs it.\n\n\
`[storage]` is read only from machine layers (built-in, system, user, environment); a \
repository's `.af/af.toml` cannot change it. See `af help storage`.",
        after_long_help = "Examples:\n  af storage\n  af storage --verbose\n  af storage --json | jq .total_bytes\n  af storage prune\n  af storage prune --apply\n  AF_STORAGE__MAX_BYTES=10GiB af storage prune --apply"
    )]
    Storage {
        /// Machine-readable af/storage@5
        #[arg(long)]
        json: bool,
        /// Also name every path that could not be measured and every Store left alone, and why
        #[arg(long, global = true)]
        verbose: bool,
        #[command(subcommand)]
        command: Option<StorageCommand>,
    },
    /// Show what Telemetry recorded on this machine: its runs, span trees, live tail and ledger
    #[command(
        display_order = 12,
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Show what Telemetry recorded on this machine: its runs, span trees, live tail and ledger.\n\n\
Telemetry records what af does while it works — spans, events and, at `content`, redacted \
content blobs — in its own store under the state root's `telemetry/`, never in a Task or \
Campaign Store. The kernel never reads it: identity, replay, the Ledger, budgets, admission and \
every Worker input are the same whether it is on, off or deleted (ADR-0153). `[telemetry] \
level` is `off`, `metrics`, `spans` (the default) or `content`, set from machine layers only; \
see `af help telemetry`.\n\n\
`status`, `runs`, `show`, `turns`, `tools`, `config`, `tail`, `ledger`, `context`, `reasoning` \
and `search` read; `reindex` recovers crashed writers' segments and \
rebuilds the index; `gc --apply` evicts telemetry under the Storage Budget's rules. None records \
telemetry, contacts a network or reads a Store. Exit codes: 0 success, 1 the telemetry \
directory cannot be read, the index is in use or a run is unknown, 2 a wrong command line or a \
refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry status\n  af telemetry runs --since 7d\n  af telemetry show RUN_ID   # needs: a recorded run (af telemetry runs)\n  af telemetry turns RUN_ID   # needs: a recorded run (af telemetry runs)\n  af telemetry tools RUN_ID   # needs: a recorded run (af telemetry runs)\n  af telemetry config RUN_ID   # needs: a recorded run (af telemetry runs)\n  af telemetry tail   # needs: a run in progress; it follows until it ends\n  af telemetry ledger RUN_ID   # needs: a recorded run (af telemetry runs)\n  af telemetry context RUN_ID ATTEMPT_ID   # needs: a recorded run and its IDs\n  af telemetry reasoning RUN_ID ATTEMPT_ID   # needs: a recorded run and its IDs\n  # needs: [telemetry] level = \"content\"\n  af telemetry search 'connection refused'\n  af telemetry gc\n  af telemetry status --json | jq .segments\n  af telemetry reindex   # needs: a telemetry directory (any recorded run)\n  af telemetry status --dir ./copied-telemetry   # needs: that copied directory\n  AF_TELEMETRY__LEVEL=content af task run TASK_ID   # needs: a planned Task"
    )]
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCommand,
    },
    /// Print a shell completion script (values complete live from the binary)
    #[command(
        display_order = 13,
        long_about = "Print a shell completion script.\n\n\
The script is thin: it asks the running `af` for candidates, so campaign names, task IDs, \
config keys, and installed versions complete from real state and the script never goes stale \
when the binary updates. `af self setup-shell --write` installs it in the shell's autoload \
directory.",
        after_long_help = "Examples:\n  \
# needs: that directory\n  af completions fish > ~/.config/fish/completions/af.fish\n  \
# needs: that directory\n  af completions zsh > ~/.local/share/zsh/site-functions/_af\n  \
source <(af completions bash)"
    )]
    Completions {
        /// The shell to generate for
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Check this machine and repository, token-free: binary, PATH, shell, tools, Providers
    #[command(
        display_order = 6,
        long_about = "Check this machine and repository, token-free: what a first run needs and \
what is missing, each with the one command that fixes it.\n\n\
Checks, each `ok`, `warn` or `fail`. A problem's `fix:` is the one command that fixes it, as a \
shell runs it, and its `hint:` says what the command cannot: a placeholder's choices (SHELL, DIR), \
a step to take first. A fix is offered only when running it can clear the check: a problem no one \
command fixes, or whose command a file that does not load would stop (an `.af/` file, the machine \
config.toml, the Provider registry), has only a hint, naming the file and the parse error's line. \
The PATH fix is the line for the shell $SHELL names, the directory quoted so nothing in it is \
expanded.\n\n\
  harness    the driving agent harness, if detected from environment markers\n\
  sandbox    whether agent sandbox markers are present\n\
  network    whether the agent sandbox reports network access; unknown without a marker\n\
  binary     the running af, its version, install receipt and how it was verified, the default link\n\
  path       the default link's directory on PATH, and whether `af` on PATH is that link\n\
  shell      completions and man pages for $SHELL\n\
  update     the cached release check: its age, a newer release, the policy, the release source\n\
  tools      git >= 2.30, curl, minisign; gh when curl is missing; what the repository's Gate runs\n\
             (make, cargo, cc, cargo-nextest, npx, python3) and a container runtime when it needs one\n\
  providers  the registry, each registered Provider's login (the token-free status probe), and\n\
             claude/codex logins found but not registered\n\
  storage    what af keeps against [storage] max_bytes, free disk against min_free_bytes, and\n\
             installs a killed installer abandoned\n\
  project    in a repository: `.af/`, the lock's af pin, each Gate command, and a Provider for\n\
             every model Worker kind\n\n\
The rows fit 100 columns, or the terminal's width when narrower (at least 60): a long detail \
or path continues on indented lines and is never cut short, and a fix command stays whole on \
its line or is broken between its arguments with ` \\`.\n\n\
It never calls a model, logs in, installs, or writes a file, and it schedules no update check; \
the charged end-to-end Provider check is `af provider doctor`. Exit codes: 0 nothing failed \
(warnings allowed), 1 at least one check failed, 2 usage error. --json prints one `af/doctor@3` \
document, each check with its `fix` and `hint`.",
        after_long_help = "Examples:\n  af doctor\n  af doctor --repo ../other-checkout   # needs: a checkout at ../other-checkout\n  af doctor --json | jq '.checks[] | select(.status != \"ok\")'"
    )]
    Doctor(MachineDoctorArgs),
    /// Preview or install the af agent pointer and skills in a Git repository
    #[command(
        display_order = 8,
        subcommand_required = true,
        arg_required_else_help = true
    )]
    Setup {
        #[command(subcommand)]
        command: SetupCommand,
    },
    /// Explain a topic or a command; `af help` lists every topic
    #[command(
        display_order = 7,
        long_about = "Explain a topic or a command.\n\n\
`af help` alone lists every topic with what it explains; start with `af help start` and \
`af help concepts`. A word that is not a topic is a command path, so `af help review run` \
equals `af review run --help`.",
        after_long_help = "Examples:\n  af help start\n  af help concepts\n  af help layers\n  af help exit-codes\n  af help errors\n  af help errors no_task_catalog\n  af help review ledger"
    )]
    Help {
        /// A topic name or a command path
        #[arg(value_name = "TOPIC|COMMAND")]
        words: Vec<String>,
        /// Print only the AGENTS.md pointer (agents topic only)
        #[arg(long)]
        snippet: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SetupCommand {
    /// Preview the af agent pointer and skills; write them only with --write
    #[command(
        long_about = "Preview the AGENTS.md pointer and both af skills in this Git repository.\n\n--write appends or updates only the marked af block in AGENTS.md and writes .claude/skills/af/SKILL.md and .agents/skills/af/SKILL.md. A foreign skill is refused. No file is written by default."
    )]
    Agents {
        /// Git repository to update [default: .]
        #[arg(long, value_name = "DIR", default_value = ".")]
        repo: PathBuf,
        /// Write the previewed files
        #[arg(long)]
        write: bool,
    },
}

/// `af doctor`.
#[derive(Debug, Args)]
pub(crate) struct MachineDoctorArgs {
    /// The repository whose `.af/`, pin and Gates to check [default: .]
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub(crate) repo: PathBuf,
    /// One `af/doctor@3` document on stdout instead of text
    #[arg(long)]
    pub(crate) json: bool,
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
    /// Explicit Campaign state directory outside the repository: absent, empty or already an
    /// af Store; it becomes af-owned, and af may remove it whole
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

/// `af provider doctor`: the selectors, mode and budgets of `af review run` that admission reads,
/// and only those. A Task file, local bindings and `render`'s node do not apply, and the Campaign
/// whose admission evidence `review run` reuses is required.
#[derive(Debug, Args, Clone)]
pub(crate) struct DoctorArgs {
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
    /// Explicit Campaign state directory outside the repository: absent, empty or already an
    /// af Store; it becomes af-owned, and af may remove it whole
    #[arg(long, value_name = "DIR", help_heading = "Selector")]
    pub(crate) state: Option<PathBuf>,
    /// The Campaign whose admission evidence `af review run --campaign` reuses
    #[arg(long, value_name = "NAME", help_heading = "Selector", add = ArgValueCompleter::new(complete_campaign))]
    pub(crate) campaign: String,
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
    /// Wall-clock budget in seconds per reviewer Attempt (default 1800), pinned in the Campaign
    /// manifest when the Campaign is captured
    #[arg(long, value_name = "N", help_heading = "Budget")]
    pub(crate) timeout_secs: Option<u64>,
    /// Wall-clock budget for each git operation
    #[arg(long, value_name = "N", help_heading = "Budget")]
    pub(crate) git_timeout_secs: Option<u64>,
    /// One JSON document on stdout instead of text
    #[arg(long, help_heading = "Output")]
    pub(crate) json: bool,
}

impl From<DoctorArgs> for RunArgs {
    fn from(args: DoctorArgs) -> Self {
        RunArgs {
            task_file: None,
            bindings: None,
            repo: args.repo,
            pipeline: args.pipeline,
            state: args.state,
            campaign: Some(args.campaign),
            policy_rev: args.policy_rev,
            base: args.base,
            candidate: args.candidate,
            uncommitted: args.uncommitted,
            focus: args.focus,
            node: None,
            heavy: args.heavy,
            restart_round: args.restart_round,
            provider: args.provider,
            provider_admission_tokens: args.provider_admission_tokens,
            provider_admission_wall_ms: args.provider_admission_wall_ms,
            timeout_secs: args.timeout_secs,
            git_timeout_secs: args.git_timeout_secs,
            json: args.json,
        }
    }
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
        after_long_help = "Examples:\n  \
# needs: Providers claude-main and codex-main (af provider setup)\n  \
af review run --policy-rev HEAD --base HEAD --uncommitted \\\n    \
--provider correctness=claude-main --provider architecture=codex-main\n  \
# needs: those Providers and a branch main\n  \
af review run --campaign pr-42 --policy-rev main --base main \\\n    --candidate HEAD \\\n    \
--provider correctness=claude-main --provider architecture=codex-main --json"
    )]
    Run(RunArgs),
    /// Show what a review would run, token-free, without Campaign state
    #[command(
        long_about = "Resolve policy, pipeline, Workers, providers, and the Subject and print the \
plan.\n\nToken-free and stateless: no sandbox, no model call, no Campaign directory. The plan is \
exactly what `run` would admit, so it is the right pre-flight for agents and CI.",
        override_usage = "af review plan [--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  # needs: one registered Provider for every model Worker\n  \
af review plan --policy-rev HEAD --base HEAD --uncommitted\n  \
# needs: one registered Provider for every model Worker\n  \
af review plan --policy-rev HEAD --base HEAD --uncommitted --json\n  \
# needs: one registered Provider for every model Worker\n  \
af review plan --policy-rev HEAD --base HEAD~1 --candidate HEAD"
    )]
    Plan(RunArgs),
    /// Compose one Worker's exact input, token-free, without Campaign state
    #[command(
        long_about = "Compose the exact input one Worker node would receive — the bytes, their \
transport, and every artifact the role-scoped manifest names — without a sandbox, a model call, \
or Campaign state. This is how a human audits what a Worker sees.",
        override_usage = "af review render --node <NODE> [--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  \
af review render --node correctness --policy-rev HEAD --base HEAD \\\n    --uncommitted\n  \
af review render --node correctness --policy-rev HEAD --base HEAD \\\n    --uncommitted --json"
    )]
    Render(RunArgs),
    /// List the ledger: findings, dispositions, and open demands
    #[command(
        after_long_help = "Examples:\n  af review ledger --campaign pr-42 --long   # needs: Campaign pr-42"
    )]
    Ledger {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Full detail per finding
        #[arg(long, help_heading = "Output")]
        long: bool,
    },
    /// Show one ledger entry by key
    #[command(
        after_long_help = "Examples:\n  # needs: Campaign pr-42 and its finding F-01H…\n  af review show --campaign pr-42 F-01H…"
    )]
    Show {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding, demand, or proposal key
        key: String,
    },
    /// Export a verified patch proposal, by id or by the finding it fixes
    #[command(
        group = ArgGroup::new("target").required(true).args(["proposal", "finding"]),
        after_long_help = "Examples:\n  # needs: Campaign pr-42 and its proposal P-01H…\n  af review export --campaign pr-42 P-01H…\n  \
# needs: Campaign pr-42 and a proposal fixing F-01H…\n  \
af review export --campaign pr-42 --finding F-01H… --allow-stale"
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
        after_long_help = "Examples:\n  af review report --campaign pr-42   # needs: Campaign pr-42\n  af review report --campaign pr-42 --format json   # needs: Campaign pr-42"
    )]
    Report {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Output format
        #[arg(long, value_enum, default_value_t = ReportFormatArg::Md, help_heading = "Output")]
        format: ReportFormatArg,
    },
    /// List Campaigns under the state root
    #[command(after_long_help = "Examples:\n  af review campaigns\n  af review campaigns --json")]
    Campaigns {
        /// State root to enumerate (default: XDG state)
        #[arg(long, value_name = "DIR")]
        state_root: Option<PathBuf>,
        /// Output format
        #[arg(long, value_enum, default_value_t = ListFormatArg::Text, help_heading = "Output")]
        format: ListFormatArg,
        /// Machine-readable af/review-campaigns@1: the same document as --format json
        #[arg(long, conflicts_with = "format", help_heading = "Output")]
        json: bool,
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
        after_long_help = "Examples:\n  \
# needs: Campaign pr-42 and its finding F-01H…\n  \
af review resolve --campaign pr-42 F-01H… rejected --policy main \\\n    --reason \"not reachable\"\n  \
# needs: Campaign pr-42 and its finding F-01H…\n  \
af review resolve --campaign pr-42 F-01H… wontfix-tracked --policy main \\\n    --reason \"tracked\" \\\n    \
--max-severity major --tracking JIRA-1 --expires-at-policy-time 12",
        // Held to the page's 100 columns, as clap does not wrap a usage line.
        override_usage = "af review resolve [OPTIONS] --campaign <NAME> --policy <REV> --reason <TEXT>\n       <KEY> <rejected|wontfix-tracked>"
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
    #[command(after_long_help = "Examples:\n  \
# needs: Campaign pr-42 and its finding F-01H…\n  \
af review attest-change --campaign pr-42 F-01H… --region src/lib.rs:10-20 \\\n    --reason \"guarded\"")]
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
        after_long_help = "Examples:\n  \
# needs: Campaign pr-42, its finding F-01H… and attestation A-01H…\n  \
af review verify-fix --campaign pr-42 F-01H… A-01H… --policy main \\\n    --reason \"reproduced clean\" \\\n    \
--positive",
        override_usage = "af review verify-fix [OPTIONS] --campaign <NAME> --policy <REV> --reason <TEXT>\n       <--positive|--negative> <FINDING> <ATTESTATION>"
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
        after_long_help = "Examples:\n  \
# needs: Campaign pr-42, a resolved finding F-01H… and evidence E-01H…\n  \
af review challenge-resolution --campaign pr-42 F-01H… --kind new-evidence \\\n    \
--reason \"repro attached\" --evidence E-01H…",
        override_usage = "af review challenge-resolution [OPTIONS] --campaign <NAME> --kind <KIND>\n       --reason <TEXT> <FINDING>"
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
    #[command(
        long_about = "Record, after a closed Round, that FROM and INTO are one defect.\n\naf never \
groups Findings by itself: two Reports at the same place with different titles or severities stay \
two Findings until an operator groups them, unless they carry one exact `rule_id` and \
`occurrence_key` or one corroborates the other's Finding (ADR-0006). Grouping keeps both Finding IDs, every \
Report and each Report's own verification; `af review ledger` then lists one Finding under INTO, \
with FROM as its alias, the higher severity and the status that blocks more, and `af review show` \
on either ID shows that group. FROM must not be grouped already and INTO must not be an alias. \
`af review ungroup FROM INTO` undoes it.",
        after_long_help = "Examples:\n  # needs: Campaign pr-42 and both findings\n  af review group --campaign pr-42 F-02H… F-01H…"
    )]
    Group {
        #[command(flatten)]
        selector: CampaignSelector,
        /// Finding to fold in
        from: String,
        /// Finding that absorbs it
        into: String,
    },
    /// Undo a grouping
    #[command(
        long_about = "Undo the grouping of FROM under INTO: both Findings are listed apart again, \
each with its own Reports, severity and status. A group whose Resolution is terminal is \
challenged first.",
        after_long_help = "Examples:\n  # needs: Campaign pr-42 and F-02H… grouped into F-01H…\n  af review ungroup --campaign pr-42 F-02H… F-01H…"
    )]
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
    /// Not a command: the browser is `af` alone at a terminal; this names it
    #[command(hide = true)]
    Tui,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PolicyTimeCommand {
    /// Advance the policy clock to a tick
    #[command(
        after_long_help = "Examples:\n  # needs: Campaign pr-42\n  af review policy-time advance --campaign pr-42 12 --reason \"sprint end\""
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
        after_long_help = "Examples:\n  # needs: Campaign pr-42, demand D-01H…\n  af review evidence add --campaign pr-42 D-01H… ./bench.txt"
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
        after_long_help = "Examples:\n  \
# needs: Campaign pr-42, its demand D-01H… and evidence E-01H…\n  \
af review evidence satisfy --campaign pr-42 D-01H… E-01H… --policy main \\\n    --reason \"bench attached\"",
        override_usage = "af review evidence satisfy [OPTIONS] --campaign <NAME> --policy <REV> --reason <TEXT>\n       <DEMAND> <EVIDENCE>"
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
    #[command(after_long_help = "Examples:\n  \
# needs: Campaign pr-42 and its demand D-01H…\n  \
af review demand waive --campaign pr-42 D-01H… --policy main \\\n    --reason \"out of scope\"")]
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
    /// Find the official CLIs and their logins on this machine, and register them with --apply
    #[command(
        long_about = "Find the official CLIs and their logins on this machine, and register them \
with --apply.\n\n\
Binaries: claude and codex on PATH first, then in ~/.local/bin, ~/.claude/local (claude), \
/opt/homebrew/bin, /usr/local/bin, ~/.npm-global/bin, ~/.volta/bin and ~/.bun/bin; one found off \
PATH is reported with the line that adds its directory to PATH for your $SHELL. No shell rc \
file, `npm prefix` or `nvm` is evaluated. Unsupported harness CLIs on PATH (gemini, cursor-agent, \
opencode, amp, copilot, aider, qwen, goose) are listed by name and version only.\n\n\
Auth directories, each with its SOURCE: env (CLAUDE_CONFIG_DIR, CODEX_HOME), default (~/.claude, \
~/.codex), sibling (~/.claude-*, ~/.config/claude, ~/.config/claude-*, ~/.codex-*, \
~/.config/codex, ~/.config/codex-*, one level, no recursion), registry (every auth_dir already \
registered) and explicit (--dir). A directory found twice is listed once. Shell aliases or \
functions that set CLAUDE_CONFIG_DIR or CODEX_HOME are not read; pass --dir for such a \
directory.\n\n\
A Claude directory holds .claude.json, .credentials.json or settings.json (the default \
~/.claude may instead have a ~/.claude.json file beside it, as `af provider status` accepts); a \
Codex directory holds auth.json or config.toml. Only those names are examined, never opened. A directory with \
none is listed `empty` and not probed unless --all, because a CLI asked about it creates files \
there; one that is a symlink, not owned by you, or writable by others is listed unsafe and not \
probed. Every other directory gets exactly the token-free login check of `af provider status`. \
LOGIN shows the plan or method (`Claude Max (claude.ai)`, `ChatGPT`, `API key`, `not logged \
in`), never an account; --identity adds the account label the CLI reported, only when stdout \
is a terminal and --json is absent.\n\n\
Nothing is written without --apply. --apply registers, in table order, every authenticated \
directory that is not registered yet, through the same locked, atomic publication as `af \
provider add`, and prints each line it ran; it never logs in, never creates a directory and \
never touches a directory that is not logged in (it prints that directory's `af provider setup \
... --login` line), and never registers a login whose CLI is off PATH, which status and Tasks \
cannot run (exit 4, with the PATH line). A registry conflict stops it at that row with exit 5; \
the rows before it stay registered, and the row suggests the retry with a free ID. A registry \
that cannot be read refuses --apply (exit 1) and makes every row's registration unknown. IDs: \
the default directory is claude-main or codex-main; a sibling is the kind and its name without \
the kind prefix (~/.config/claude-work is claude-work, ~/.codex-gridgain is codex-gridgain); \
the env directory follows the sibling rule when its name is a sibling name, else claude-env or \
codex-env; --dir follows the sibling rule, and needs one --id per --dir when \
more than one names the same kind; a taken ID gets -2, -3; *-ambient is never generated.\n\n\
--json prints one af/provider-detect@4 document; its `install` names the install command and \
documentation of each kind whose CLI was not found, and `next_action` the scan to run again once \
one is installed (null when a CLI was found). Exit codes: 0 the scan completed, 1 \
unclassified failure, 2 usage error, 4 a found CLI cannot start or is incompatible, or a login \
was left unregistered because its CLI is off PATH (the rows are still printed), 5 a registry \
conflict stopped --apply, 8 the host denied a capability probe before compatibility or login \
could be established.",
        after_long_help = "Examples:\n  af provider detect\n  af provider detect --apply\n  af provider detect --json\n  af provider detect --dir /secure/claude-team --kind claude --apply\n  af provider detect --all"
    )]
    Detect {
        /// Also scan this auth directory (repeatable)
        #[arg(long = "dir", value_name = "PATH", action = ArgAction::Append)]
        dirs: Vec<PathBuf>,
        /// The ID to register each --dir under, in --dir order (repeatable)
        #[arg(long = "id", value_name = "ID", action = ArgAction::Append, requires = "dirs")]
        ids: Vec<String>,
        /// Scan one Provider kind only; names the kind of a --dir without a clear marker
        #[arg(long, value_enum)]
        kind: Option<ProviderKindArg>,
        /// Also probe directories that hold no login marker (the CLI may create files there)
        #[arg(long)]
        all: bool,
        /// Register every authenticated, unregistered directory found
        #[arg(long)]
        apply: bool,
        /// Emit the versioned `af/provider-detect@4` document instead of the table
        #[arg(long)]
        json: bool,
        /// Add the account label each CLI reported (terminal only, never with --json)
        #[arg(long)]
        identity: bool,
    },
    /// Disable a registered Provider locally until a wall-clock expiry, without probing it
    #[command(
        long_about = "Disable a registered Provider on this machine without probing it.\n\n--for accepts a positive integer with s, m, h or d (maximum 30d). Expiry survives restarts. New probes and private sends are refused; already-running calls may finish. Older pinned releases do not honor this state.",
        after_long_help = "Example:\n  af provider disable claude-personal --for 2d"
    )]
    Disable {
        /// The registered Provider ID to disable
        id: String,
        /// How long: a positive integer with s, m, h or d (at most 30d)
        #[arg(long = "for", value_name = "DURATION")]
        duration: String,
    },
    /// Enable a Provider or clear a removed Provider's disable entry, without probing it
    Enable {
        /// The Provider ID to enable, or a removed one whose disable entry to clear
        id: String,
    },
    /// Show the provider registry and each harness login, without a model call
    #[command(
        alias = "list",
        long_about = "Show the provider registry and each harness login, without a model call. \
`af provider list` is the same command.\n\n\
Fast by default: the registry, a token-free check that each context's CLI accepts af's \
invocation, and one bounded authentication check per context. Subscription \
and quota probes are opt-in behind --usage, and an unavailable usage probe never demotes an \
authenticated Provider — it exits 7 and says so. End-to-end usability stays with the charged \
`af provider doctor`.\n\n\
A Provider whose official CLI is missing, not executable, or exits non-zero on its own version \
check is reported as an installation failure, with the CLI's own first error line and the fix \
it suggests on stderr. One whose help page does not declare every option af passes is reported \
as incompatible, with its version, the missing options and the upgrade command on stderr. A \
host permission denial is `probe_denied`, exit 8: retry outside the sandbox or with \
host-approved permissions.\n\n\
A context that does not exist or holds no login marker is not probed: its CLI would create \
files there. Each row that needs a login names one fix, `af provider setup ID --kind KIND \
--auth-dir DIR --login`; --verbose adds the CLI's own login command. With no registry entry, \
directories `af provider detect` would list are named on one line, unprobed; with no CLI at \
all, the install commands are printed.\n\n\
Exit codes: 0 fine, 4 a Provider CLI cannot start or is incompatible, 7 optional usage \
unavailable, 8 a host-denied probe.",
        after_long_help = "Examples:\n  af provider status\n  af provider status --json\n  af provider status --usage\n  af provider status --verbose"
    )]
    Status {
        /// Emit the versioned `af/provider-status@4` document instead of the table
        #[arg(long)]
        json: bool,
        /// Also probe subscription and quota windows (slower, and may be unavailable)
        #[arg(long)]
        usage: bool,
        /// Also print the official CLI's own login command for each row without a login
        #[arg(long)]
        verbose: bool,
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
        after_long_help = "Examples:\n  af provider models\n  \
# needs: Provider claude-work (af provider setup)\n  af provider models claude-work --json\n  \
# needs: Provider claude-work\n  af provider models claude-work --resolve 'Fable 5.1/xhigh'\n  \
# needs: Provider codex-main\n  af provider models codex-main --resolve 'GPT-6 Sol/ultra'"
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
--auth-dir names an existing directory; only --login creates a missing one, because a login \
needs a home.\n\n\
Exit codes: 0 registered, 3 human action required, 4 Provider CLI missing, cannot start or \
incompatible, 5 \
registry conflict, 6 authentication failed, 8 host-denied capability probe. Re-running setup \
for the same binding is safe.",
        after_long_help = "Examples:\n  \
af provider setup codex-main --kind codex   # needs: the codex CLI, logged in\n  \
# needs: the codex CLI, logged in\n  af provider setup codex-main --kind codex --json\n  \
# needs: the claude CLI and a private terminal for its login\n  \
af provider setup claude-work --kind claude --auth-dir /secure/claude-work \\\n    --login"
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
        /// Emit the versioned non-interactive `af/provider-setup@2` document
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
The directory is probed first, as `af provider status` probes it, and a context that is not \
authenticated is refused with exit 6 and the `af provider setup ID --kind KIND --auth-dir DIR \
--login` line that logs in there. --unverified registers it without the probe and prints \
`warning: not verified here`.\n\n\
Exit codes: 0 registered, 4 Provider CLI missing, cannot start or incompatible, 5 registry \
conflict, 6 not authenticated.",
        after_long_help = "Examples:\n  af provider add codex-main --kind codex   # needs: a codex login in ~/.codex\n  \
# needs: that directory\n  af provider add claude-work --kind claude --auth-dir /secure/claude-work\n  \
# needs: the directory /secure/ci\n  \
af provider add claude-ci --kind claude --auth-dir /secure/ci --unverified"
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
        /// Register without asking the Provider CLI whether the directory holds a login
        #[arg(long)]
        unverified: bool,
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
        after_long_help = "Examples:\n  af provider remove codex-main   # needs: Provider codex-main (af provider add)\n  \
af provider remove claude-work codex-work   # needs: both registered"
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
        after_long_help = "Examples:\n  \
# needs: Campaign pr-42 and Provider claude-main (af provider setup)\n  \
af provider doctor --campaign pr-42 --provider correctness=claude-main"
    )]
    Doctor(DoctorArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProviderAuthCommand {
    /// Begin or reuse one login; a private host grant is required before the CLI starts
    #[command(
        override_usage = "af provider auth begin [OPTIONS] --kind <KIND> --auth-dir <DIR>\n       --host-read-fd <HOST_READ_FD> --host-write-fd <HOST_WRITE_FD> <ID>"
    )]
    Begin {
        /// Provider ID to log in and register
        id: String,
        /// The Provider's CLI kind
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
        /// Provider ID the login session belongs to
        id: String,
        /// The Provider's CLI kind
        #[arg(long, value_enum)]
        kind: ProviderKindArg,
        /// The session's native auth directory
        #[arg(long, value_name = "DIR")]
        auth_dir: PathBuf,
    },
    /// Cancel only the currently identified recovery session (idempotent)
    Cancel {
        /// Provider ID the login session belongs to
        id: String,
        /// The Provider's CLI kind
        #[arg(long, value_enum)]
        kind: ProviderKindArg,
        /// The session's native auth directory
        #[arg(long, value_name = "DIR")]
        auth_dir: PathBuf,
        /// The recovery session to cancel, as `status` names it
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
    /// Runner profile for the generated Workers [default: the Provider kinds registered on
    /// this machine: claude, codex, or mixed for both or none]
    #[arg(long, value_enum, help_heading = "Generate")]
    pub(crate) runner: Option<RunnerArg>,
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
    /// Also run the token-free Provider status probe for the providers block (it starts each
    /// registered Provider's CLI to check its login)
    #[arg(long, help_heading = "Output")]
    pub(crate) probe: bool,
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
        /// The starter set to write (a starter name, not an installed Task-kind profile)
        #[arg(long, default_value="document", value_parser=["document", "software", "planning", "all", "experiment", "report"])]
        profile: String,
        /// Existing minisign public key assigned to developer owner; enables the Planner
        #[arg(long)]
        developer_public_key: Option<PathBuf>,
        /// Directory the destination is relative to
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Absent directory, as a safe path relative to --repo (an absolute path is refused)
        #[arg(long)]
        destination: String,
        /// One JSON document on stdout instead of text
        #[arg(long)]
        json: bool,
    },
    /// Check exact Pipeline/Worker contract fixtures from a local Git revision without dispatch
    Test {
        /// Local Git checkout holding the catalog
        #[arg(long)]
        source: PathBuf,
        /// Git revision whose committed catalog is checked
        #[arg(long, default_value = "HEAD")]
        revision: String,
        /// Catalog manifest path in that revision
        #[arg(long, default_value = "catalog.toml")]
        manifest: String,
        /// Fixture path in the same Git commit (defaults beside the manifest)
        #[arg(long)]
        fixtures: Option<String>,
        /// Check only this Pipeline's fixtures
        #[arg(long, conflicts_with = "worker")]
        pipeline: Option<String>,
        /// Check only this Worker's fixtures
        #[arg(long)]
        worker: Option<String>,
        /// One JSON document on stdout instead of text
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
        after_long_help = "Examples:\n  \
# needs: that Task package\n  af catalog digest .af/task-packages/team/implementer\n  \
# needs: that Task package\n  \
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
        /// Git revision to capture the catalog from
        #[arg(long, default_value = "HEAD")]
        revision: String,
        /// Catalog manifest path in that revision
        #[arg(long, default_value = "catalog.toml")]
        manifest: String,
        /// Directory the destination is relative to
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Absent directory, as a safe path relative to --repo (an absolute path is refused)
        #[arg(long)]
        destination: String,
        /// One JSON document on stdout instead of text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum TaskCommand {
    /// Capture a Task and preview its plan, as `plan` does; with --execute, also run it
    #[command(
        long_about = "Capture a Task and show its execution plan before any Worker runs.\n\nWithout \
--execute this is `af task plan` under another name: the same capture, the same preview, nothing \
dispatched. Inspect the compact ASCII preview, or use `af task explain TASK_ID --tree` for the expanded hierarchy. Run with `--confirm-plan PLAN_ID` after reviewing the captured plan. `--execute` explicitly opts into immediate automation. JSON output also previews by default. Generated plans still require signed developer approval.\n\nExecution never writes to the original repository, commits, pushes, or delivers; see `af task deliver`.",
        after_long_help = "Examples:\n  \
# needs: a Task file; the same as af task plan\n  af task start --file ../ticket.json\n  \
af task explain TASK_ID --tree   # needs: a Task\n  \
# needs: a planned Task and its plan ID\n  af task run TASK_ID --confirm-plan sha256:...\n  \
# needs: a Task file and its Providers\n  af task start --file ../ticket.json --execute --json"
    )]
    Start {
        /// Versioned Task JSON/TOML file, processed by the common Task runtime
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
        /// Explicitly execute immediately instead of stopping at the captured plan preview
        #[arg(long)]
        execute: bool,
        /// With --execute: print every line of the storage sweep that ends the run, not only its
        /// one summary line
        #[arg(long, requires = "execute")]
        verbose: bool,
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
        /// Explicit state directory outside the repository: absent, empty or already an af
        /// Store; it becomes af-owned, and af may remove it whole
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
    /// Capture and compile a Task file without dispatching any Worker (`start` without --execute)
    #[command(
        long_about = "Capture a Task file and compile its plan without dispatching any Worker: \
token-free, and nothing runs. `af task start` without --execute does the same. The Task is \
recorded in its Store with the plan; `af task run TASK_ID --confirm-plan PLAN_ID` runs exactly \
that plan, and `af task explain TASK_ID` shows it again.\n\nNever: calls a model, runs a check, \
writes to the repository, commits or pushes.",
        after_long_help = "Examples:\n  \
# needs: a Task file (af catalog init writes starters)\n  af task plan --file ../ticket.json\n  \
af task plan --file ../ticket.json --uncommitted --json   # needs: a Task file"
    )]
    Plan {
        /// Versioned Task JSON/TOML file to capture (keep it outside the checkout)
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
        /// Capture local Worker tuning for this Task (default: the Workspace's bindings file)
        #[arg(long, value_name = "FILE")]
        bindings: Option<PathBuf>,
        /// Explicit machine-local read-only issue source accounts
        #[arg(long, value_name = "FILE", requires = "file")]
        source_bindings: Option<PathBuf>,
        /// Repository to work in
        #[arg(
            long,
            value_name = "DIR",
            default_value = ".",
            help_heading = "Selector"
        )]
        repo: PathBuf,
        /// Explicit state directory outside the repository: absent, empty or already an af
        /// Store; it becomes af-owned, and af may remove it whole
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
        /// Source revision whose committed `.af/` and files the Task captures (default: HEAD)
        #[arg(
            long,
            value_name = "REV",
            default_value = "HEAD",
            conflicts_with = "uncommitted",
            help_heading = "Selector"
        )]
        authority: String,
        /// Capture the working tree instead of a revision
        #[arg(long, help_heading = "Selector")]
        uncommitted: bool,
        /// When the Task's limits are too small for the Pipeline, plan with the smallest limits
        /// that fit instead of refusing, and print every limit it raised
        #[arg(long)]
        fit: bool,
        /// Re-plan an unstarted Task of the same ID from a changed Task file, in place
        #[arg(long)]
        replace: bool,
        /// One JSON document on stdout instead of text
        #[arg(long, help_heading = "Output")]
        json: bool,
    },
    /// Capture an updated issue and replace its plan without executing Workers
    Refresh {
        /// Task id
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
    #[command(
        override_usage = "af task decision-payload [OPTIONS] --developer <LABEL> --decision <DECISION>\n       --reason <TEXT> --output <ABSENT_FILE> <TASK_ID>"
    )]
    DecisionPayload {
        /// Task id
        task_id: String,
        /// The captured developer label whose key will sign the payload
        #[arg(long, value_name = "LABEL")]
        developer: String,
        /// The decision the payload records
        #[arg(long, value_parser = ["approved", "rejected"])]
        decision: String,
        /// Why, recorded in the signed payload
        #[arg(long, value_name = "TEXT")]
        reason: String,
        /// Where to write the payload bytes; the file must not exist
        #[arg(long, value_name = "ABSENT_FILE")]
        output: PathBuf,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Write one recorded Task output to stdout or an absent file: Markdown or JSON
    #[command(
        long_about = "Write one recorded output of a finished Task: to stdout, or with --output \
to a file that must not exist yet (nothing is ever overwritten). The port is one of the Task's \
output ports, as `af task show TASK_ID` lists them, holding a single artifact.\n\n--json \
describes the written file as one `af.task-output/1` document, so it needs --output; the bytes \
alone go to stdout without it.",
        after_long_help = "Formats:\n  json                the recorded artifact envelope, for machines and audit\n  markdown            a Document or report as a person reads it: its sections, sources,\n                      plain-language result and matching review; derived, not itself\n                      reviewed. Other Markdown outputs (an optimization report, a\n                      comparison) as their own renderer writes them\n  canonical-markdown  a Document or report exactly as recorded: the bytes the checks and\n                      the reviewer judged\n  human-markdown      the same as markdown, for a Document or report\n\nAn unknown --port is refused with the ports the Task's result has.\n\nExamples:\n  \
# needs: a finished report Task\n  \
af task output TASK_ID --port report --format markdown\n  \
# needs: a finished report Task\n  \
af task output TASK_ID --port report --format markdown --output report.md\n  \
# needs: a finished report Task\n  \
af task output TASK_ID --port report --format canonical-markdown \\\n    --output report-canonical.md"
    )]
    Output {
        /// Task id
        task_id: String,
        /// The output port to write, as `af task show TASK_ID` lists them
        #[arg(long, value_name = "PORT")]
        port: String,
        /// json, markdown, canonical-markdown or human-markdown (see Formats below)
        #[arg(long, default_value="json", value_parser=["json","markdown","canonical-markdown","human-markdown"])]
        format: String,
        /// Write to this file, which must not exist, instead of stdout
        #[arg(long, value_name = "ABSENT_FILE")]
        output: Option<PathBuf>,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Record an exact outer-plan or pending experiment approval signed by a captured developer key
    Approve {
        /// Task id
        task_id: String,
        /// The payload `af task decision-payload` wrote
        #[arg(long, value_name = "FILE")]
        payload: PathBuf,
        /// The developer's minisign signature over that payload
        #[arg(long, value_name = "FILE")]
        signature: PathBuf,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Record an exact plan rejection signed by a captured developer key
    Reject {
        /// Task id
        task_id: String,
        /// The payload `af task decision-payload` wrote
        #[arg(long, value_name = "FILE")]
        payload: PathBuf,
        /// The developer's minisign signature over that payload
        #[arg(long, value_name = "FILE")]
        signature: PathBuf,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Run or resume a Task's recorded plan, or continue a reviewed Task with --from
    #[command(
        long_about = "Run or resume a captured Task using its exact recorded plan and \
authority, or with --from TASK continue a finished reviewed Task as a new Task.\n\n--confirm-plan names the plan you reviewed (`af task plan`, `start` or `explain` print it); a \
plan that changed since refuses. --execute runs the current plan without that confirmation, for \
automation. Model Workers spend tokens within the plan's budget.\n\nWhile it waits it prints on stderr one line per node that starts or finishes, with \
the time since the run started and the tokens charged so far, and every 5 minutes a heartbeat \
(`af task run: implement running 23 min, 412k tokens, host load 11.2`); --quiet keeps them \
back. stderr's last line is always the Task's state and the next command: `Task ID: \
phase/outcome/cause; next: COMMAND` (`af help task-states`).\n\n--detach starts the same run \
in the background and returns once it holds the Task and its plan is admitted, printing its pid \
and its log, `runs/<TASK_ID>-<N>.log` in the Task Store (`af/task-detached@2` under --json); a \
run not admitted within 2 minutes exits 4 with `admitted: false`, and `af task status` is next. \
The run has no terminal, survives the shell that started it, and is the same process a \
foreground run is; `af task wait`, `af task status` and `af task cancel` follow it. Every run \
records its process in the Store, `runs/<TASK_ID>-<N>.json`, before it takes the Task.\n\n\
--from TASK plans NEW_ID as a new Task that continues the finished Task TASK from its reviewed \
candidate (ADR-0212): the candidate Snapshot of its last review round (--round N for another) is \
the new source, that round's reviewer findings and evaluator verdict are the implementer's \
`findings` input, --goal FILE replaces the goal, and TASK's Task file is the rest. Without \
--execute it prints the plan for --confirm-plan, as `af task plan` does; --from runs in the \
foreground and cannot be combined with --detach.\n\nResuming: a run that stopped \
before the Task finished (Ctrl-C, SIGTERM, a closed lid, a killed process, a lost writer lease) \
resumes with `af task run TASK_ID`. It waits for a dead run's lease to expire (at most 60 s), \
and its first line says what it does: the interrupted node's Attempt is settled as abandoned, \
charged only the usage its Provider reported, and dispatched again without using one of the \
node's Attempts, once per node unless its slot declares `recovery_attempts`. When a node has \
used that allowance, the first line says `cannot continue Task TASK_ID` with the count and the \
fix. On macOS the run holds a power assertion (caffeinate), so the machine does not \
idle-sleep.\n\nNever: writes to the original repository, commits, pushes, or delivers; see \
`af task deliver`.",
        after_long_help = "Examples:\n  \
# needs: a planned Task and the plan ID it printed\n  af task run TASK_ID --confirm-plan PLAN_ID\n  \
# needs: a Task whose `next:` line names --retry-gate\n  af task run TASK_ID --retry-gate\n  \
# needs: a Task whose run was interrupted (`interrupted by SIGINT`)\n  af task run TASK_ID\n  \
af task run TASK_ID --execute --detach   # needs: a planned Task\n  \
af task run TASK_ID --execute --quiet --json   # needs: a planned Task\n  \
af task run NEW_ID --from TASK_ID   # needs: a finished reviewed Task TASK_ID\n  \
# needs: the same, goal.md\n  af task run NEW_ID --from TASK_ID --round 1 --goal goal.md --execute"
    )]
    Run {
        /// Task id
        task_id: String,
        /// Plan TASK_ID as a new Task that continues the finished Task TASK from its reviewed
        /// candidate: that review round's candidate Snapshot is the new source, its reviewer
        /// findings and evaluator verdict the implementer's `findings` input, and TASK's Task
        /// file the rest. Without --execute the plan is printed for --confirm-plan
        #[arg(
            long,
            value_name = "TASK",
            conflicts_with_all = ["confirm_plan", "retry_gate", "detach"],
            help_heading = "Continue"
        )]
        from: Option<String>,
        /// The review round of --from TASK to continue; omitted, its last round
        #[arg(
            long,
            value_name = "N",
            requires = "from",
            value_parser = clap::value_parser!(u32).range(1..=16),
            help_heading = "Continue"
        )]
        round: Option<u32>,
        /// A file whose text replaces the continued Task's goal; omitted, the goal stays
        #[arg(
            long,
            value_name = "FILE",
            requires = "from",
            help_heading = "Continue"
        )]
        goal: Option<PathBuf>,
        /// Local Provider bindings for the new Task, as `af task plan --bindings` takes them
        #[arg(
            long,
            value_name = "FILE",
            requires = "from",
            help_heading = "Continue"
        )]
        bindings: Option<PathBuf>,
        /// Confirm the exact plan shown by plan/start/explain; stale identities refuse
        #[arg(long, value_name = "PLAN_ID", conflicts_with = "execute")]
        confirm_plan: Option<String>,
        /// Explicit automation opt-in: run the current plan without preview confirmation
        #[arg(long)]
        execute: bool,
        /// Run the failed gate of a finished Task again on the same sealed candidate: a new
        /// check Attempt in the same round under the recorded plan, nothing re-planned or
        /// re-admitted; the Task continues into review when the check passes. Refused when the
        /// Task's last failure was not a gate
        #[arg(long, conflicts_with = "confirm_plan")]
        retry_gate: bool,
        /// Print every line of the storage sweep that ends the run, not only its one summary
        /// line (`storage: freed 1.2 GiB (2 warm keys)`). Say on stderr how the run keeps the
        /// machine awake: the power assertion it holds on macOS, or why it holds none
        #[arg(long)]
        verbose: bool,
        /// Run in the background: return once the run holds the Task, with its pid and log
        #[arg(long)]
        detach: bool,
        /// Print no progress lines or heartbeat on stderr; the last line still follows
        #[arg(long)]
        quiet: bool,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Block until a Task settles, then exit with its exit code
    #[command(
        long_about = "Block until a Task settles — no writer holds it and no run of this host \
is starting — then print its status line (`af/task-status@1` under --json) and exit with the \
Task's exit code: its outcome's (`af help exit-codes`), 4 for a Task that waits for a person. \
With --timeout S it gives up after S seconds, prints the status so far and exits 124. A \
planned Task with no run in progress is refused (exit 1): there is nothing to wait for.\n\n\
Never: starts, resumes or changes a Task.",
        after_long_help = "Examples:\n  \
af task wait TASK_ID   # needs: a Task af task run --detach started\n  \
af task wait TASK_ID --timeout 600 --json   # needs: a running Task"
    )]
    Wait {
        /// Task id
        task_id: String,
        /// Give up after S seconds and exit 124
        #[arg(long, value_name = "S", value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Print a Task's state, current node, elapsed time and tokens so far, and what is next
    #[command(
        long_about = "Print where a Task is, now, without waiting: its state \
(phase/outcome/cause, `af help task-states`), the node whose Attempt runs, the time since its \
clock started, the tokens charged so far, the run that holds it (run number, pid and host) and \
who cancelled it, then the next command on its own line, within 100 columns (the terminal's \
width when narrower) and broken only between arguments. --json prints one `af/task-status@1` \
document. Exits 0: the state is data.",
        after_long_help = "Examples:\n  \
af task status TASK_ID   # needs: a Task\n  \
af task status TASK_ID --json   # needs: a Task"
    )]
    Status {
        /// Task id
        task_id: String,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Stop a running Task's `af task run` and its Worker processes; the Task stays resumable
    #[command(
        long_about = "Stop the `af task run` that holds a Task, and with it the Task's Worker \
processes, which that run stops as it does on Ctrl-C. The process is the one the Task's writer \
lease names, found in the Store's run records (`runs/<TASK_ID>-<N>.json`) by the lease's \
writer: only that recorded pid is signalled (SIGTERM, and once more after 60 s), never a \
process found by its name. A writer on another host is refused with its pid and host.\n\n\
The request is recorded first, with who asked (`runs/<TASK_ID>-<N>.cancel.json`), so the Task \
shows `abandoned/cancelled/host_cancelled` and `af task show` names the requester. The Task \
stays resumable: `af task run TASK_ID` continues it. --json prints one `af/task-cancelled@1` \
document. A Task no writer holds is refused (exit 1).",
        after_long_help = "Examples:\n  \
af task cancel TASK_ID   # needs: a running Task\n  \
af task cancel TASK_ID --json   # needs: a running Task"
    )]
    Cancel {
        /// Task id
        task_id: String,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Print the review findings of a Task's last (or Nth) review round and the evaluator's verdict
    #[command(
        long_about = "Print the review findings of a Task's last review round, or of the Nth \
with --round N: per reviewer, each finding with its severity, confidence, title, location, the \
quoted evidence and the reviewer's proposed fix, grouped by severity; then the evaluator's \
verdict on that round, the one the Task log recorded after it, with its reason, and the next \
command. A round whose reduce step did not run shows each \
reviewer's reports as recorded, not gathered. A Task that recorded no review round says so, \
with the state it reached.\n\n--json prints one `af/task-findings@1` document. Exits 0 once \
it read the Store.\n\nNever: dispatches a Worker, contacts a Provider or writes the Store.",
        after_long_help = "Examples:\n  \
af task findings TASK_ID   # needs: a reviewed Task\n  \
af task findings TASK_ID --round 1 --json   # needs: a reviewed Task"
    )]
    Findings {
        /// Task id
        task_id: String,
        /// The Nth recorded review round instead of the last (1 is the first)
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
        round: Option<u64>,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Make a Git worktree's tracked tree equal to a Task's candidate Snapshot
    #[command(
        long_about = "Make the tracked tree of the Git worktree DIR equal to a Task's candidate \
Snapshot: its result's Snapshot, or the last one a node of its plan sealed, or the one --snapshot \
names (one the Task recorded; `af task diff` shows them). Files are written, deleted and given \
their executable bit; untracked files the Snapshot does not name stay. Then it prints the \
Snapshot, how many paths it wrote, deleted and changed only in mode, and the next commands \
(`af/task-materialized@1` under --json).\n\nRefused: a DIR with uncommitted changes, unless \
--force; a candidate the Task did not verify, unless --allow-unverified, which then says \
`unverified candidate: review it before merging`.\n\nNever: commits, stages, pushes, \
contacts a remote or writes the Store. `af task deliver` keeps its stricter contract: a \
verified Task, onto a new branch and worktree.",
        after_long_help = "Examples:\n  \
# needs: a verified Task and a worktree\n  af task materialize TASK_ID --into ../review-wt\n  \
# needs: a reviewed Task and a clean worktree at ../review-wt\n  \
af task materialize TASK_ID --into ../review-wt --allow-unverified --json"
    )]
    Materialize {
        /// Task id
        task_id: String,
        /// A Snapshot the Task recorded, instead of its candidate
        #[arg(long, value_name = "SHA")]
        snapshot: Option<String>,
        /// The top of the Git worktree to write
        #[arg(long, value_name = "DIR")]
        into: PathBuf,
        /// Write a candidate the Task did not verify
        #[arg(long)]
        allow_unverified: bool,
        /// Write even when DIR has uncommitted changes, overwriting them
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Print a Task's change as a unified diff, touching no worktree
    #[command(
        long_about = "Print the change from a Task's base Snapshot (its source, or --base) to its \
candidate (its result's Snapshot, the last one a node sealed, or --snapshot) as the unified diff \
`git diff` prints for the same two trees, renames detected; --stat and --name-only print what \
`git diff` prints with them. Both Snapshots must be ones the Task recorded. --json prints one \
`af/task-diff@1` document: the files with their status and line counts, and the patch.\n\n\
Never: touches a worktree, writes the Store or contacts a remote.",
        after_long_help = "Examples:\n  \
af task diff TASK_ID   # needs: a Task with a candidate\n  \
af task diff TASK_ID --stat   # needs: a Task with a candidate\n  \
# needs: a Task with a candidate\n  af task diff TASK_ID --json | jq '.files[].path'"
    )]
    Diff {
        /// Task id
        task_id: String,
        /// The Snapshot to show instead of the candidate
        #[arg(long, value_name = "SHA")]
        snapshot: Option<String>,
        /// The Snapshot to diff from instead of the Task's source
        #[arg(long, value_name = "SHA")]
        base: Option<String>,
        /// Only the diffstat
        #[arg(long, conflicts_with_all = ["name_only", "json"])]
        stat: bool,
        /// Only the changed paths
        #[arg(long, conflicts_with = "json")]
        name_only: bool,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Explain a captured Task's ports, hierarchy, bindings, coverage and budgets
    #[command(
        long_about = "Explain a captured Task's ports, hierarchy, bindings, coverage and budgets: \
the plan `af task run --confirm-plan` would run, read from its Store. Token-free; records \
nothing.",
        after_long_help = "Examples:\n  \
af task explain TASK_ID   # needs: a planned Task\n  \
af task explain TASK_ID --tree   # needs: a planned Task\n  \
# needs: a Task and one of its plan IDs\n  af task explain TASK_ID --plan PLAN_ID --json"
    )]
    Explain {
        /// Task id
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
    #[command(
        long_about = "Export the executable definition a Task's plan produced (a generated plan, \
once its Planner ran) as a shared catalog directory: `catalog.toml`, `contracts.json` and the \
Pipeline and Worker packages, under one package name. The destination, a safe path under \
--repo, must not exist; nothing is overwritten, and nothing exported is authorized to execute \
until a catalog that names it is reviewed and committed.",
        after_long_help = "Examples:\n  \
# needs: a planned Task\n  \
af task export TASK_ID --name team/pagination --destination exported\n  \
# needs: a planned Task\n  \
af task export TASK_ID --name team/pagination --destination exported --json"
    )]
    Export {
        /// Task id
        task_id: String,
        /// The package name the exported definitions carry, such as `team/pagination`
        #[arg(long, value_name = "NAME")]
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
it is reported complete.\n\nA verified Task of kind `verification` whose source it bound from \
another Task's candidate (`inputs.source = {task, port}`) and left unchanged delivers that \
candidate on the other Task's committed source, and the delivery records both Tasks. A \
candidate no Task verified is never delivered: `af task materialize TASK_ID --into DIR \
--allow-unverified` writes it into a worktree you choose.\n\nNever: mutates the current \
checkout, commits on your behalf, pushes, opens a pull request, or contacts a remote.",
        after_long_help = "Examples:\n  \
# needs: a verified Task\n  \
af task deliver TASK_ID --repo . --branch af/TASK_ID --worktree ../TASK_ID \\\n    --confirm TASK_ID \\\n    \
--json"
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
    #[command(
        override_usage = "af task observe-adoption [OPTIONS] --workload <WORKLOAD> --model <MODEL>\n       --environment <ENVIRONMENT> <TASK_ID>"
    )]
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
        after_long_help = "Examples:\n  \
# needs: a Task Store (af task plan creates one)\n  af task gc --older-than 14 --keep 5\n  \
af task gc --older-than 14 --keep 5 --apply   # needs: a Task Store\n  \
af task gc --older-than 0 --keep 10 --json   # needs: a Task Store"
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
    /// Show one Task: its state, why it ended, its checks, spend and the next command
    #[command(
        long_about = "Show one Task: its state (phase/outcome/cause, `af help task-states`), why \
it ended, its Plan, warm checks, failed nodes with their first error line and the artifact IDs \
of their whole diagnostic and stderr, and as the last line the one command to run next.\n\n\
--checks prints the gate's last check receipt instead: each check's status, duration, warm or \
cold, its warm layer (key, lock, source groups, build and test phases, an eviction and its \
cause), the step that stopped it, its failing and flaky tests and the last 40 lines of its \
stderr; with --json, one `af/task-checks@2` document.",
        after_long_help = "Examples:\n  af task show pagination-cli   # needs: Task pagination-cli\n  \
af task show pagination-cli --checks   # needs: Task pagination-cli\n  \
af task show pagination-cli --json   # needs: Task pagination-cli"
    )]
    Show {
        /// Task id
        task_id: String,
        /// Print the gate's last check receipt: each check, its failing tests and its stderr
        #[arg(long)]
        checks: bool,
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
<!-- /af-task-report -->\n\n--json prints one `af/task-report@2` document instead. Tasks \
appear in the order given, numbered as rounds. Active time is the sum of the Task's runs, the \
writer leases that started an Attempt or recorded a gate retry (`af task run --retry-gate`), so \
waiting between runs is not counted; wall time runs \
from its first to its last recorded event. A figure the Store does not record is shown as \
unknown.\n\n--pr-body prints the whole pull request description instead, which `gh pr create \
--body-file` takes unchanged: the template (--template, or the checkout's \
.github/PULL_REQUEST_TEMPLATE.md) with this block between its markers, and the lines added \
block, between `<!-- af-pr-lines:v1 -->` and `<!-- /af-pr-lines -->`, that the checkout's \
scripts/pr-lines.py prints when it has one (counted from --base REF, origin/main by default); \
without a template, the two blocks under their headings. scripts/check-pr-report.py accepts it. \
When the checkout has scripts/pr-lines.py and it cannot print its block, nothing is printed and \
the command exits 1; so does a block scripts/check-pr-report.py would refuse, naming what is \
missing. A whole block is the begin marker, then the title `**Lines added: <figure>**`, the \
header `| Directory | Added | Share |`, its separator and at least one directory row, each \
directory once, or `**Lines added: none**` alone, then the end marker.\n\nNever: dispatches a \
Worker, contacts a Provider, writes the Store or changes a Task. Providers appear only as kind \
and model; no Provider label, path, credential, prompt or Worker output is printed.",
        after_long_help = "Examples:\n  af task report pagination-cli   # needs: Task pagination-cli\n  \
# needs: Tasks implement-x and verify-x\n  af task report implement-x verify-x --json\n  \
# needs: Tasks implement-x and verify-x\n  \
af task report implement-x verify-x --pr-body > body.md && gh pr create \\\n    --body-file body.md"
    )]
    Report {
        /// Task ids, reported in this order
        #[arg(value_name = "TASK_ID", required = true)]
        task_ids: Vec<String>,
        /// Print the whole pull request description: the template with this block, and the
        /// lines added block `scripts/pr-lines.py` prints when the checkout has the script
        #[arg(long, conflicts_with = "json")]
        pr_body: bool,
        /// The pull request template [default: .github/PULL_REQUEST_TEMPLATE.md of --repo]
        #[arg(long, value_name = "FILE", requires = "pr_body")]
        template: Option<PathBuf>,
        /// The base the lines added block counts from, passed to scripts/pr-lines.py
        /// [default: origin/main]
        #[arg(long, value_name = "REF", requires = "pr_body")]
        base: Option<String>,
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
        after_long_help = "Examples:\n  af project brief\n  af project brief --variant orient --json | jq .manifest\n  \
# needs: the ID of a Survey (af project history)\n  af project brief --survey sha256:… | wc -c"
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
        after_long_help = "Examples:\n  \
af project workspace new research\n  \
af project workspace new review-2 --branch agent/review-2 --from main\n  \
af project workspace new spike --path ../spike"
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
        after_long_help = "Examples:\n  \
# needs: a worktree of this repository at ../afactory-wt-research, and $W\n  \
af project workspace add ../afactory-wt-research --label research \\\n    --state \"$W/state\" \\\n    \
--bindings \"$W/bindings.toml\" --tasks \"$W/tasks\""
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
        after_long_help = "Examples:\n  \
# needs: Workspace research (af project workspace new)\n  af project workspace remove research\n  \
af project workspace remove spike --delete-worktree   # needs: Workspace spike"
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
Prints the configured `[telemetry] level`; the telemetry directory and its bytes (the \
allocated bytes of its regular files, a hard-linked file once, directories and links nothing), in \
total against `[telemetry] max_bytes` and per entry (each segment with its manifest, the blobs, \
the index with its WAL and shared memory, temporary files, locks, and `unrecognized`: every \
other path beneath the directory, a symbolic link included, counted once, named up to 16, never \
read, followed or removed, so the total is every file's allocated bytes beneath it), each sealed \
segment \
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
unchanged). Paths under the home directory are written `~/…`, under `--json` too.\n\n\
Never: recovers or removes a segment, records telemetry, reads a Store, reads around an index of \
another schema version, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or its index is of another \
schema version (`af telemetry reindex` replaces it), 2 a refused `[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry status\n  af telemetry status --json\n  # needs: that copied directory\n  af telemetry status --dir /backup/telemetry --json | jq .bytes.total"
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
        after_long_help = "Examples:\n  af telemetry reindex   # needs: a telemetry directory (any recorded run)\n  # needs: a telemetry directory (any recorded run)\n  af telemetry reindex --json\n  af telemetry reindex --dir ./copied-telemetry   # needs: that copied directory"
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
        after_long_help = "Examples:\n  af telemetry runs\n  af telemetry runs --since 7d --kind task\n  af telemetry runs --json | jq '.runs[] | {run_id, active_us, history}'\n  # needs: that copied directory\n  af telemetry runs --dir ./copied-telemetry --json"
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
        after_long_help = "Examples:\n  af telemetry show RUN_ID   # needs: a recorded run (af telemetry runs)\n  # needs: a recorded run (af telemetry runs)\n  af telemetry show RUN_ID --depth 2\n  # needs: a recorded run and its IDs\n  af telemetry show RUN_ID --attempt ATTEMPT_ID\n  # needs: a recorded run and its IDs\n  af telemetry show RUN_ID --span SPAN_ID --json\n  # needs: a recorded run and its IDs\n  af telemetry show RUN_ID --store STORE_ID"
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
    /// One run's model turns, one row each: timing by arrival, the harness's usage, stop reason, sizes
    #[command(
        long_about = "One run's model turns, one row each: timing by arrival, the harness's usage, stop reason, sizes.\n\n\
RUN is the kernel run id `af telemetry runs` lists. At `spans` and above Telemetry taps each \
harness's stream as it arrives (Claude `stream-json`, Codex `--json`): every Claude `assistant` \
message and every Codex turn is an `af.turn`, timed by the arrival of the lines that opened and \
closed it, with the harness's own per-turn usage (exact decimal strings, the `turn` series, \
never added into the Attempt's or the run's totals, which are the kernel's), its stop reason \
from a closed set, the bound model when the harness reported it as the model that answered, \
the number and bytes of its text and thinking blocks and its tool calls — never their words, \
which wait for the content level and Telemetry's release decision. Each row names its harness \
process, Attempt and node, and whether that stream can be analysed whole: `complete`, \
`incomplete` (the observer dropped chunks, a gap, a frame too long to judge) or `open` (the tap \
has not ended). `--attempt ID` keeps one Attempt's turns. When two Stores record the same run id \
it lists both and asks for `--store ID`.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or the run or Attempt is \
unknown, 2 a wrong command line, a run two Stores share without `--store`, or a refused \
`[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry turns RUN_ID   # needs: a recorded run (af telemetry runs)\n  # needs: a recorded run and its IDs\n  af telemetry turns RUN_ID --attempt ATTEMPT_ID\n  # needs: a recorded run (af telemetry runs)\n  af telemetry turns RUN_ID --json \\\n    | jq '.turns[] | {ordinal, duration_us, usage}'"
    )]
    Turns {
        /// The kernel run id
        #[arg(value_name = "RUN")]
        run: String,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        store: Option<String>,
        /// Only this Attempt's turns
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        attempt: Option<String>,
        /// Machine-readable af/telemetry-turns@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// One run's tool calls, one row each: name, timing by arrival, outcome, input and output bytes
    #[command(
        long_about = "One run's tool calls, one row each: name, timing by arrival, outcome, input and output bytes.\n\n\
RUN is the kernel run id `af telemetry runs` lists. Every Claude `tool_use` block is an \
`af.tool` from its line's arrival to the arrival of the `tool_result` that answers it, and \
every Codex `command_execution` or `mcp_tool_call` item one from `item.started` to \
`item.completed`; each row carries the tool's name when it is in the invocation's own tool set \
(`(unlisted)` otherwise), its correlation id, its turn, its outcome (`succeeded`, `failed`, \
`incomplete`), the bytes of its arguments and of its result, and a command's exit code and an \
item's status — never the arguments or the result themselves. Each row names its harness \
process, Attempt and node, and whether that stream can be analysed whole (`complete`, \
`incomplete`, `open`). `--attempt ID` keeps one Attempt's tool calls. When two Stores record \
the same run id it lists both and asks for `--store ID`.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or the run or Attempt is \
unknown, 2 a wrong command line, a run two Stores share without `--store`, or a refused \
`[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry tools RUN_ID   # needs: a recorded run (af telemetry runs)\n  # needs: a recorded run and its IDs\n  af telemetry tools RUN_ID --attempt ATTEMPT_ID\n  # needs: a recorded run (af telemetry runs)\n  af telemetry tools RUN_ID --json \\\n    | jq '.tools[] | {name, duration_us, output_bytes}'"
    )]
    Tools {
        /// The kernel run id
        #[arg(value_name = "RUN")]
        run: String,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        store: Option<String>,
        /// Only this Attempt's tool calls
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        attempt: Option<String>,
        /// Machine-readable af/telemetry-tools@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// One Attempt's composed Worker input as the content level kept it: the prompt bytes and the manifest
    #[command(
        long_about = "One Attempt's composed Worker input as the content level kept it: the prompt bytes and the manifest.\n\n\
RUN is the kernel run id `af telemetry runs` lists and ATTEMPT the kernel Attempt id (`af telemetry \
show RUN` names it). At `[telemetry] level = \"content\"`, and only there, the kernel keeps the \
composed Worker input of `af.context.compose` as two blobs: the prompt bytes the Worker received \
(`gen_ai.content.input`) and the context manifest they were composed by (`gen_ai.content.system`). \
Each was redacted whole before any truncation: a line or value that looks like credential \
material is withheld (the artifact and Snapshot ids of the request's values, and the identity \
members of the manifest, stay), and content past `content_max_blob_bytes` keeps its head and \
tail. Prints the Attempt's compose span, the manifest digest, the rendered size and layers, \
each blob's occurrence, digest and sizes (its source digest is over the bytes the Worker \
received), then the prompt itself; `--raw` prints only the prompt's retained bytes, exactly. \
Below `content` it says the content was not captured, naming the level; an exhausted \
`content_attempt_max_bytes` or a missing blob file is reported for the occurrence. The content \
is the operator's: no Worker ever reads it. When two Stores record the same run id it lists \
both and asks for `--store ID`.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or the run or Attempt is \
unknown, 2 a wrong command line, a run two Stores share without `--store`, or a refused \
`[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry context RUN_ID ATTEMPT_ID   # needs: a recorded run and its IDs\n  # needs: a recorded run and its IDs\n  af telemetry context RUN_ID ATTEMPT_ID --raw > prompt.json\n  # needs: a recorded run and its IDs\n  af telemetry context RUN_ID ATTEMPT_ID --json \\\n    | jq '{state, manifest, input: .input.digest}'"
    )]
    Context {
        /// The kernel run id
        #[arg(value_name = "RUN")]
        run: String,
        /// The kernel Attempt id
        #[arg(value_name = "ATTEMPT")]
        attempt: String,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        store: Option<String>,
        /// Print only the prompt's retained bytes, exactly as kept
        #[arg(long, conflicts_with = "json", help_heading = "Output")]
        raw: bool,
        /// Machine-readable af/telemetry-context@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// One Attempt's thinking blocks and reasoning summaries, in the order they arrived
    #[command(
        long_about = "One Attempt's thinking blocks and reasoning summaries, in the order they arrived.\n\n\
RUN is the kernel run id `af telemetry runs` lists and ATTEMPT the kernel Attempt id. At \
`[telemetry] level = \"content\"` with `content_reasoning = true`, a harness's Claude \
`thinking` blocks and Codex `reasoning` items are pended while its process runs and, once \
Telemetry's own release decision over the complete streams withheld nothing, kept as blobs \
named by `af.content.reasoning` on their turn, redacted whole (grant values replaced, credential \
lines withheld) before any truncation and counted against `content_attempt_max_bytes`. Prints \
each of the Attempt's harness processes with the level it recorded at and whether its reasoning \
was `captured`, `not_captured` (below `content`, or `content_reasoning = false`), `withheld` \
(the release decision found login-challenge material or the adapter replaced the streams) or \
`open` (not yet released), then every thought in order with its turn, occurrence, digest and \
sizes and its retained text. The content is the operator's: no Worker, Warm Set, Worker Notes \
or optimization source ever reads it. When two Stores record the same run id it lists both and \
asks for `--store ID`.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or the run or Attempt is \
unknown, 2 a wrong command line, a run two Stores share without `--store`, or a refused \
`[telemetry]` key.",
        after_long_help = "Examples:\n  af telemetry reasoning RUN_ID ATTEMPT_ID   # needs: a recorded run and its IDs\n  # needs: a recorded run and its IDs\n  af telemetry reasoning RUN_ID ATTEMPT_ID --json \\\n    | jq '.thoughts[] | {ordinal, kind, turn_ordinal}'"
    )]
    Reasoning {
        /// The kernel run id
        #[arg(value_name = "RUN")]
        run: String,
        /// The kernel Attempt id
        #[arg(value_name = "ATTEMPT")]
        attempt: String,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", help_heading = "Selector")]
        store: Option<String>,
        /// Machine-readable af/telemetry-reasoning@1
        #[arg(long, help_heading = "Output")]
        json: bool,
        /// Read this telemetry directory instead of the configured one (one copied from another machine)
        #[arg(long, value_name = "PATH", help_heading = "Selector")]
        dir: Option<PathBuf>,
    },
    /// Full-text search over the content Telemetry kept: matching blobs, their span paths, a bounded excerpt
    #[command(
        long_about = "Full-text search over the content Telemetry kept: matching blobs, their span paths, a bounded excerpt.\n\n\
TEXT is searched as one phrase (no query operators) in the FTS5 table `content_fts`, which \
every `af telemetry` command keeps at ingest over the retained text blobs when \
`[telemetry] content_index = true`: composed Worker inputs, assistant text, reasoning, tool \
arguments and results, check output, Store event payloads, Finding bodies and configuration \
texts, each as redacted when it was kept. Prints each matching blob's digest with a bounded \
excerpt of its redacted text and its occurrences — the event that names it, its time, run, \
Attempt and span path. `--since D` keeps occurrences of the last D (30m, 12h, 14d, 2w); \
`--run RUN` keeps one run's (`--store ID` when two Stores record it). Search exists only at \
`content`: below it, or with `content_index = false`, it is refused, because nothing \
searchable is kept.\n\n\
Never: reads a Store, records telemetry, removes anything, or contacts a network.\n\n\
Exit codes: 0 success, 1 the telemetry directory cannot be read or the run is unknown, 2 a \
wrong command line, a run two Stores share without `--store`, a refused `[telemetry]` key, or a \
level below `content` or `content_index = false`.",
        after_long_help = "Examples:\n  # needs: [telemetry] level = \"content\"\n  af telemetry search 'connection refused'\n  af telemetry search 'TODO' --since 7d   # needs: [telemetry] level = \"content\"\n  # needs: a recorded run (af telemetry runs)\n  af telemetry search 'flaky' --run RUN_ID --json \\\n    | jq '.matches[].occurrences[].span_path'"
    )]
    Search {
        /// The text to find, as one phrase
        #[arg(value_name = "TEXT")]
        text: String,
        /// Only occurrences of the last D (30m, 12h, 14d, 2w)
        #[arg(long, value_name = "D", help_heading = "Selector")]
        since: Option<String>,
        /// Only this run's occurrences (the kernel run id)
        #[arg(long, value_name = "RUN", help_heading = "Selector")]
        run: Option<String>,
        /// The Store the run belongs to, when two Stores record it (`af.store_id`)
        #[arg(long, value_name = "ID", requires = "run", help_heading = "Selector")]
        store: Option<String>,
        /// Machine-readable af/telemetry-search@1
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
        after_long_help = "Examples:\n  af telemetry config RUN_ID   # needs: a recorded run (af telemetry runs)\n  # needs: a recorded run and its IDs\n  af telemetry config RUN_ID --execution EXECUTION_ID\n  # needs: a recorded run (af telemetry runs)\n  af telemetry config RUN_ID --json \\\n    | jq '.rows[] | select(.layer == \"environment\")'"
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
        after_long_help = "Examples:\n  af telemetry tail   # needs: a run in progress; it follows until it ends\n  af telemetry tail RUN_ID   # needs: a recorded run (af telemetry runs)\n  # needs: a recorded run (af telemetry runs)\n  af telemetry tail RUN_ID --json | jq -c 'select(.record == \"span_end\")'"
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
        after_long_help = "Examples:\n  af telemetry ledger RUN_ID   # needs: a recorded run (af telemetry runs)\n  # needs: a recorded run (af telemetry runs)\n  af telemetry ledger RUN_ID --json | jq '.findings[] | {key, status}'\n  # needs: a recorded run (af telemetry runs)\n  af telemetry ledger RUN_ID --json | jq .accounting.totals\n  # needs: that copied directory\n  af telemetry ledger RUN_ID --dir ./copied-telemetry"
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
index deleted for the next reader to rebuild when there is no room), measured as the allocated \
bytes of the files removed; registered directories that no longer exist are dropped from the registry. Without \
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
    /// What is installed, the pin that applies here, and the last release check
    #[command(
        long_about = "What is installed, the default, its receipt, the pin that applies here, and the last release check.\n\n\
The `last check:` line says when a release check last answered and the newest releases it found; \
when the last attempt failed it adds `last attempt <time> failed: <reason>`. A pre-release user \
sees the newest stable release and the newest pre-release apart, with `af self update --rc` when \
a newer pre-release exists. A check that has held its lock for over an hour is reported with the \
command that clears it. When a check is due, status schedules the detached check as every other \
command does; `--refresh` runs it now instead, before printing (never offline: AF_SELF_OFFLINE=1 \
skips it).",
        after_long_help = "Examples:\n  af self status\n  af self status --refresh\n  af self status --json | jq .last_error"
    )]
    Status {
        /// Run the release check now, before printing, instead of scheduling it when due
        #[arg(long)]
        refresh: bool,
        /// One JSON document on stdout (af/self-status@2)
        #[arg(long)]
        json: bool,
    },
    /// Install a newer release as the default (or just check for one)
    #[command(
        long_about = "Install a newer release as the default.\n\nDownloads the release for this \
machine's target, verifies it against the release's SHA256SUMS (signed with the release key; \
the signature is required), installs it beside the other versions, and retargets the default \
symlink atomically. The previous version stays installed for `af self rollback`.\n\n\
`--check` only asks: exit 10 when a newer release exists, 0 when none does, 1 with the reason \
when the release source cannot be asked (the anonymous route's HTTP status or curl error comes \
before any `gh auth login` hint). With AF_SELF_OFFLINE=1 it asks nothing, prints `offline: check \
skipped` and exits 0. A pre-release af also hears of a newer pre-release, which `--rc` installs. \
Every network call is bounded: 5 s to connect and 30 s to list releases.\n\n\
Never: changes what a pinned project runs, activates a release older than 0.8.0, or stores a \
token.",
        after_long_help = "Examples:\n  \
af self update --check   # exits 10 when a newer release exists\n  \
af self update   # needs: the network and an af installed by af self\n  \
# needs: the network and an af installed by af self\n  af self update --version 0.9.0"
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
--write, writes only into the shell's autoload directory and the man directory, then says what \
is left to do: nothing for fish, which picks the completions up in a new shell; the rc line \
above for the others. --remove deletes exactly the completion file and the af man pages it \
would write. rc files are never edited. Supported shells: bash, zsh, fish, elvish, powershell.",
        after_long_help = "Examples:\n  af self setup-shell\n  af self setup-shell --shell zsh --write\n  af self setup-shell --remove"
    )]
    SetupShell {
        /// Shell to set up (default: $SHELL)
        #[arg(long, value_enum)]
        shell: Option<Shell>,
        /// Write the files instead of printing the plan
        #[arg(long, conflicts_with = "remove")]
        write: bool,
        /// Remove the completion file and the af man pages instead
        #[arg(long)]
        remove: bool,
    },
    /// Remove every version this receipt installed, and the default symlink
    #[command(
        long_about = "Remove every version this receipt installed, and the default symlink.\n\n\
Without --purge it keeps af's config, state and cache, and names the shell completion files and \
man pages `af self setup-shell --write` installed, which stay; --purge removes those too, with \
af's config, state, cache and data directories, every registered Store, and the Claude history \
af's Workers left in each registered Claude config directory.",
        after_long_help = "Examples:\n  \
af self uninstall   # needs: an af installed by af self\n  \
af self uninstall --purge   # needs: an af installed by af self"
    )]
    Uninstall {
        /// Also remove af's config, state, cache, and data directories, the shell completions
        /// and man pages, every registered Store, and the Claude history af's Workers left in
        /// each registered Claude config directory
        #[arg(long)]
        purge: bool,
    },
    /// Capture project history and preview economics or an explicitly installed experiment
    #[command(
        long_about = "Capture declared, project-associated history and preview an Optimization Task. With a captured .af/optimization-policy.json, --strategy light runs the installed diagnose/propose/experiment path; otherwise the command retains token-free report-only analysis. Every generated experimental closure requires a separate signed decision. Raw transcripts, protected case bodies, private reasoning and undeclared paths are never sent to diagnosis/proposal Workers. The command is dispatched through the project's pinned af release.",
        after_long_help = "Examples:\n  \
af self optimize --strategy light   # needs: .af/optimization-sources.toml\n  \
af self optimize --since 30d   # needs: .af/optimization-sources.toml\n  \
# needs: that file and a report pipeline\n  af self optimize --all-history --execute\n  \
# needs: .af/optimization-policy.json\n  af self optimize --experiment --execute\n  \
# needs: the Task ID af self optimize printed\n  \
af task output TASK_ID --port report --format markdown \\\n    --output optimization.md"
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
        /// Explicit common Task state directory outside the repository; it becomes af-owned
        #[arg(long, value_name = "DIR")]
        state: Option<PathBuf>,
        /// One JSON document on stdout
        #[arg(long)]
        json: bool,
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    /// `af review --help` lists its subcommands by lifecycle in a template rather than clap's
    /// flat list; every visible subcommand is there exactly once, with its own summary.
    #[test]
    fn the_review_help_groups_every_subcommand_once_with_its_summary() {
        let root = Af::command();
        let review = root.find_subcommand("review").unwrap();
        let mut listed = 0;
        for sub in review.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            let about = sub.get_about().unwrap().to_string();
            let line = format!("  {:<22}{about}\n", sub.get_name());
            assert_eq!(
                REVIEW_HELP_TEMPLATE.matches(&line).count(),
                1,
                "{line:?} in the review help template"
            );
            listed += 1;
        }
        assert_eq!(listed, 18);
        let rows = REVIEW_HELP_TEMPLATE
            .lines()
            .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
            .count();
        assert_eq!(rows, listed, "a row for a subcommand that does not exist");
    }

    /// `af provider doctor` takes the selectors admission reads and nothing a Task file,
    /// bindings or `render` take.
    #[test]
    fn provider_doctor_takes_only_its_own_flags() {
        let root = Af::command();
        let doctor = root
            .find_subcommand("provider")
            .and_then(|provider| provider.find_subcommand("doctor"))
            .unwrap();
        let flags: Vec<&str> = doctor
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            .collect();
        for absent in ["file", "bindings", "node"] {
            assert!(!flags.contains(&absent), "--{absent} in {flags:?}");
        }
        for present in ["campaign", "provider", "policy-rev", "base", "json"] {
            assert!(flags.contains(&present), "--{present} missing in {flags:?}");
        }
        let campaign = doctor
            .get_arguments()
            .find(|arg| arg.get_long() == Some("campaign"))
            .unwrap();
        assert!(campaign.is_required_set());
    }

    /// `af self --help` leads with what most people come for.
    #[test]
    fn self_help_leads_with_status_and_update() {
        let root = Af::command();
        let names: Vec<&str> = root
            .find_subcommand("self")
            .unwrap()
            .get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(|sub| sub.get_name())
            .collect();
        assert_eq!(names[..2], ["status", "update"]);
        assert_eq!(names.last(), Some(&"optimize"));
    }

    /// Every argument of every visible command says what it is: `--help` is a command's
    /// contract (ADR-0146), and a blank line in it is a flag nobody can use safely.
    #[test]
    fn every_argument_of_every_command_has_a_description() {
        fn walk(command: &clap::Command, path: &str, blank: &mut Vec<String>) {
            for arg in command.get_arguments() {
                if arg.is_hide_set() || ["help", "version"].contains(&arg.get_id().as_str()) {
                    continue;
                }
                let help = arg.get_help().or(arg.get_long_help());
                if help.is_none_or(|help| help.to_string().trim().is_empty()) {
                    blank.push(format!("{path} {}", arg.get_id()));
                }
            }
            for sub in command.get_subcommands() {
                if !sub.is_hide_set() {
                    walk(sub, &format!("{path} {}", sub.get_name()), blank);
                }
            }
        }
        let mut root = super::Af::command();
        root.build();
        let mut blank = Vec::new();
        walk(&root, "af", &mut blank);
        assert!(blank.is_empty(), "undocumented arguments: {blank:#?}");
        // The walk reaches the commands this rule was written for.
        for path in [
            ["task", "plan"],
            ["task", "run"],
            ["task", "explain"],
            ["task", "output"],
            ["task", "export"],
        ] {
            let task = root.find_subcommand(path[0]).unwrap();
            let sub = task.find_subcommand(path[1]).unwrap();
            assert!(sub.get_arguments().count() > 1, "{path:?}");
        }
    }
}
