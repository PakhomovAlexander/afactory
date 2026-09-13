//! The `af` command tree: one clap definition feeds parsing, `--help`, `af help <topic>`, shell
//! completions, and the man pages, so none of them can drift from the others.
//!
//! Help follows three rules. `af` alone shows the namespaces; `af <namespace>` shows only that
//! namespace; `af <command> --help` says what the command does, what it never does, its options
//! grouped by role, and examples. Every existing flag and `--json` shape is unchanged from the
//! hand-written parser this replaced.

use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use clap_complete::engine::ArgValueCompleter;

use crate::selfmgmt::{complete_campaign, complete_version};

pub(crate) const REVIEW_PIPELINE: &str = ".af/pipelines/review.toml";
pub(crate) const IMPLEMENT_PIPELINE: &str = ".af/pipelines/implement.toml";

const AF_ABOUT: &str = "Afactory — deterministic multi-agent review and implementation";
const AF_LONG_ABOUT: &str = "\
af drives sandboxed, budgeted Worker attempts over an immutable Snapshot of a git repository \
and folds their typed results into a findings ledger. Project policy lives in `.af/` (committed \
TOML); state lives outside the repository under XDG directories; every model call is bounded \
and journaled.

Namespaces:
  review     plan, run, and work a review Campaign
  task       start, inspect, and deliver an implement Task
  provider   inspect and preflight the configured model providers
  onboard    generate or validate `.af/` review authority for a repository
  config     show the effective configuration and where each value came from
  self       install, update, roll back, and remove af itself

`af help <topic>` explains config, layers, environment, exit-codes, json, self, and trust.";

const AF_AFTER_HELP: &str = "\
Exit codes:
  0 pass · 1 error · 2 usage · 3 fail · 4 incomplete · 10 outdated (self update --check)

Examples:
  af onboard --apply             generate `.af/` for this repository (token-free preview first)
  af review plan                 what a review would run, without spending a token
  af review --uncommitted        review the working tree against HEAD
  af self status                 what is installed and which pin applies here";

#[derive(Debug, Parser)]
#[command(
    name = "af",
    bin_name = "af",
    about = AF_ABOUT,
    long_about = AF_LONG_ABOUT,
    after_long_help = AF_AFTER_HELP,
    disable_help_subcommand = true,
    disable_version_flag = true,
    arg_required_else_help = true,
    max_term_width = 100
)]
pub(crate) struct Af {
    /// Print the version (add --json for commit, target, and receipt)
    #[arg(long, short = 'V', global = false)]
    pub(crate) version: bool,
    /// With --version: one JSON document instead of a line
    #[arg(long, requires = "version")]
    pub(crate) json: bool,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Command {
    /// Plan, run, and work a review Campaign
    #[command(
        long_about = "Plan, run, and work a review Campaign.\n\n\
A Campaign reviews one Subject (a diff or the working tree) against committed `.af/` policy. \
`plan` is token-free. `run` executes the pipeline inside sandboxes and folds results into the \
ledger; the remaining commands read or resolve that ledger. Flags given directly to `af review` \
are shorthand for `af review run`.",
        after_long_help = "Examples:\n  af review plan --json\n  af review --uncommitted\n  af review run --campaign pr-42 --heavy\n  af review ledger --campaign pr-42\n  af review report --campaign pr-42 --format md",
        override_usage = "af review <COMMAND>\n       af review [--light|--heavy] [RUN OPTIONS]   (shorthand for `af review run`)",
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
that registry and the machine's harness logins without contacting any model. `doctor` runs a \
bounded, charged preflight for the bindings you name.",
        after_long_help = "Examples:\n  af provider status\n  af provider doctor --provider correctness=claude-code"
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
  * --refresh-lock: explicitly recompute only the selected pipeline and referenced Worker pins.\n\
  * With legacy .review/ and no .af/: preview the `.af/` it becomes — pipelines with format \
upgrades applied, the reviewer packages they reference byte for byte, a project file, a lock; \
--migrate --apply \
writes it (absent-only) and leaves .review/ for you to delete. Since v0.8.0 `.review/` is no \
longer read for new Campaigns; scaffolding .af/ beside it is refused.\n\
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
        after_long_help = "Runner profiles:\n  mixed   correctness = Claude Opus/high; architecture = machine-configured Codex (default)\n  claude  both Workers = Claude Opus/high\n  codex   both Workers = machine-configured Codex\n\nGate discovery prefers `make check`, then `scripts/verify.sh`, Rust, Go, or a package-manager test script. If none is unambiguous, pass a trusted literal.\n\nExamples:\n  af onboard\n  af onboard --gate 'check=make check' --apply\n  af onboard --runner mixed --apply\n  af onboard --refresh-lock\n  af onboard --refresh-lock --af 0.9.0     move the pin to 0.9.0\n  af onboard --migrate --apply"
    )]
    Onboard(OnboardArgs),
    /// Start, inspect, and deliver an implement Task
    #[command(
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Start, inspect, and deliver an implement Task.\n\n\
A Task runs one sequential implement pipeline — implementer, read-only Gates, independent \
evaluator — over a captured source Snapshot and ends at a verified or unverified derived \
Snapshot. Nothing is written back to the repository unless you `deliver` it, to a new local \
branch and worktree, after confirming the Task ID.",
        after_long_help = "Examples:\n  af task start --kind implement --goal \"describe the change\" --json\n  af task list --json\n  af task show TASK_ID\n  af task deliver TASK_ID --repo . --branch af/TASK_ID --worktree ../TASK_ID --confirm TASK_ID"
    )]
    Task {
        #[command(subcommand)]
        command: TaskCommand,
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
    /// Install, update, roll back, and remove af itself
    #[command(
        name = "self",
        subcommand_required = true,
        arg_required_else_help = true,
        long_about = "Install, update, roll back, and remove af itself.\n\n\
Installed versions live under `$XDG_DATA_HOME/af/versions/<v>/` with a receipt each; the \
default is the `~/.local/bin/af` symlink. A project whose `.af/af.lock` pins another version \
is run by that version: `af` execs it, installing it on demand when the archive matches the \
digest the lock records. Outside a lock, releases are verified against their signed \
`SHA256SUMS`. Updates are checked by a detached, rate-limited child and applied by the policy \
in `[self]` (see `af help self`).\n\n\
Never: touches a binary it did not install, stores a token, changes the version a pinned \
project runs, or activates a release older than 0.7.1 (the first that can update itself).",
        after_long_help = "Examples:\n  af self status\n  af self update --check\n  af self update\n  af self rollback\n  af self setup-shell --write"
    )]
    SelfCmd {
        #[command(subcommand)]
        command: SelfCommand,
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
    /// Explain a topic (config, layers, environment, exit-codes, json, self, trust) or a command
    #[command(
        long_about = "Explain a topic or a command.\n\n\
Topics: config, layers, environment, exit-codes, json, self, trust. Anything else is treated \
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

/// Selector, mode, budget, and output options shared by `plan`, `run`, `tui`, and `provider
/// doctor`.
#[derive(Debug, Args, Clone)]
#[command(group = ArgGroup::new("mode").args(["light", "heavy"]))]
pub(crate) struct RunArgs {
    /// Versioned Review Task file, executed through the shared Task runtime
    #[arg(long = "file", value_name = "FILE", help_heading = "Selector", conflicts_with_all = ["pipeline", "campaign", "base", "candidate", "focus", "node", "light", "heavy", "restart_round", "provider", "resume_provider", "git_timeout_secs"])]
    pub(crate) task_file: Option<PathBuf>,
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
    #[arg(
        long,
        value_name = "REV",
        help_heading = "Selector",
        conflicts_with = "authority"
    )]
    pub(crate) policy_rev: Option<String>,
    /// Diff base revision
    #[arg(
        long,
        value_name = "REV",
        help_heading = "Selector",
        conflicts_with = "authority"
    )]
    pub(crate) base: Option<String>,
    /// Candidate revision to review (default: HEAD)
    #[arg(
        long,
        value_name = "REV",
        help_heading = "Selector",
        conflicts_with = "uncommitted"
    )]
    pub(crate) candidate: Option<String>,
    /// Compatibility alias: one revision as both policy and diff base
    #[arg(long, value_name = "REV", help_heading = "Selector")]
    pub(crate) authority: Option<String>,
    /// Review the working tree (including unstaged changes) against HEAD
    #[arg(long, help_heading = "Selector")]
    pub(crate) uncommitted: bool,
    /// Free-text focus handed to every reviewer
    #[arg(long, value_name = "TEXT", help_heading = "Selector")]
    pub(crate) focus: Option<String>,
    /// `render`: the Worker node whose exact input to compose
    #[arg(long, value_name = "NODE", help_heading = "Selector")]
    pub(crate) node: Option<String>,
    /// One bounded Round with the pipeline's light selection (default)
    #[arg(long, help_heading = "Mode")]
    pub(crate) light: bool,
    /// The pipeline's complete convergence policy
    #[arg(long, help_heading = "Mode")]
    pub(crate) heavy: bool,
    /// Abandon the open Round and start a fresh one
    #[arg(long, help_heading = "Mode")]
    pub(crate) restart_round: bool,
    /// Bind a pipeline node to a provider from the registry
    #[arg(long, value_name = "NODE=PROVIDER_ID", action = ArgAction::Append, help_heading = "Providers")]
    pub(crate) provider: Vec<String>,
    /// Resume a fenced provider operation at the given epoch
    #[arg(long, value_name = "OPERATION_ID:EPOCH", action = ArgAction::Append, help_heading = "Providers")]
    pub(crate) resume_provider: Vec<String>,
    /// Wall-clock budget for the whole run
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
        override_usage = "af review run [--light|--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  af review run --uncommitted\n  af review run --campaign pr-42 --policy-rev main --base main --candidate HEAD\n  af review run --heavy --provider correctness=claude-code --json"
    )]
    Run(RunArgs),
    /// Show what a review would run, token-free, without Campaign state
    #[command(
        long_about = "Resolve policy, pipeline, Workers, providers, and the Subject and print the \
plan.\n\nToken-free and stateless: no sandbox, no model call, no Campaign directory. The plan is \
exactly what `run` would admit, so it is the right pre-flight for agents and CI.",
        override_usage = "af review plan [--light|--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  af review plan\n  af review plan --uncommitted --json"
    )]
    Plan(RunArgs),
    /// Compose one Worker's exact input, token-free, without Campaign state
    #[command(
        long_about = "Compose the exact input one Worker node would receive — the bytes, their \
transport, and every artifact the role-scoped manifest names — without a sandbox, a model call, \
or Campaign state. This is how a human audits what a Worker sees.",
        override_usage = "af review render --node <NODE> [--light|--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  af review render --node correctness\n  af review render --node correctness --uncommitted --json"
    )]
    Render(RunArgs),
    /// Drive a Campaign interactively
    #[command(
        override_usage = "af review tui [--light|--heavy] [OPTIONS]",
        after_long_help = "Examples:\n  af review tui --campaign pr-42"
    )]
    Tui(RunArgs),
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
    Status,
    /// Run a bounded, charged preflight for the named bindings
    #[command(
        long_about = "Run a bounded, charged preflight for the named provider bindings.\n\n\
Resolves the review selector exactly as `af review plan` would, then sends each bound provider \
one fenced preflight operation and reports identity, model, and spend.",
        after_long_help = "Examples:\n  af provider doctor --provider correctness=claude-code"
    )]
    Doctor(RunArgs),
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
    /// Repin the selected pipeline and its Workers after a reviewed edit
    #[arg(long, help_heading = "Action")]
    pub(crate) refresh_lock: bool,
    /// Move legacy `.review/` policy to `.af/` (preview; add --apply to write)
    #[arg(long, help_heading = "Action", conflicts_with = "refresh_lock")]
    pub(crate) migrate: bool,
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
pub(crate) enum TaskCommand {
    /// Start an implement Task from a goal
    #[command(
        long_about = "Start an implement Task.\n\nCaptures the source Snapshot (HEAD, an explicit \
--authority, or the working tree with --uncommitted), runs the sequential implement pipeline, and \
records a verified or unverified derived Snapshot. Exit 3 when the evaluator does not accept.\n\n\
Never: writes to the repository, commits, pushes, or delivers — see `af task deliver`.",
        after_long_help = "Examples:\n  af task start --kind implement --goal \"add --json to ledger\" --json\n  af task start --kind implement --goal \"…\" --uncommitted --timeout-secs 1800"
    )]
    Start {
        /// Task kind (v2 supports exactly `implement`)
        #[arg(long, value_parser = ["implement"], value_name = "KIND", required_unless_present = "file", conflicts_with = "file")]
        kind: Option<String>,
        /// What the implementer must achieve
        #[arg(
            long,
            value_name = "TEXT",
            required_unless_present = "file",
            conflicts_with = "file"
        )]
        goal: Option<String>,
        /// Versioned Task JSON/TOML file, processed by the common Task runtime
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Repository to work in
        #[arg(
            long,
            value_name = "DIR",
            default_value = ".",
            help_heading = "Selector"
        )]
        repo: PathBuf,
        /// Pipeline definition, relative to the repository
        #[arg(long, value_name = "FILE", default_value = IMPLEMENT_PIPELINE, help_heading = "Selector", conflicts_with = "file")]
        pipeline: PathBuf,
        /// Explicit state directory (outside the repository)
        #[arg(long, value_name = "DIR", help_heading = "Selector")]
        state: Option<PathBuf>,
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
        #[arg(long, value_name = "DIR", default_value = ".")]
        repo: PathBuf,
        #[arg(long, value_name = "DIR")]
        state: Option<PathBuf>,
        #[arg(long, default_value = "HEAD", conflicts_with = "uncommitted")]
        authority: String,
        #[arg(long)]
        uncommitted: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run or resume a captured Task using its exact recorded plan and authority
    Run {
        task_id: String,
        #[command(flatten)]
        inspect: TaskInspectArgs,
    },
    /// Explain a captured Task's ports, hierarchy, bindings, coverage and budgets
    Explain {
        task_id: String,
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
        /// One JSON document on stdout instead of text
        #[arg(long, help_heading = "Output")]
        json: bool,
    },
    /// List Tasks with outcome and spend
    List {
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
    /// One JSON document on stdout instead of text
    #[arg(long, help_heading = "Output")]
    pub(crate) json: bool,
}

// ------------------------------------------------------------------------------------------
// config · self · completions

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
    /// What is installed, the default, its receipt, and the pin that applies here
    Status {
        /// One JSON document on stdout
        #[arg(long)]
        json: bool,
    },
    /// Install a newer release as the default (or just check for one)
    #[command(
        long_about = "Install a newer release as the default.\n\nDownloads the release for this \
machine's target, verifies it against the release's SHA256SUMS (signed with the release key \
since 0.8.0; the signature is required), installs it beside the other versions, and retargets \
the default symlink atomically. The previous version stays installed for `af self rollback`.\n\n\
Never: changes what a pinned project runs, activates a release older than 0.7.1, or stores a \
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
    /// Install an exact version without making it the default
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
        /// Also remove af's config, state, cache, and data directories
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
