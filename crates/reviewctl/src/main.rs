//! `af review` - reviews from a definition file to a verdict, and the campaign loop.
//!
//! The review namespace has six subcommands:
//!
//! - `run` captures the repository HEAD as an immutable snapshot, loads the pipeline through
//!   its lockfile, binds each packaged reviewer to the adapter its runner names, executes
//!   under the definition's budgets, and prints what happened — every node, every finding,
//!   the spend, the verdict. With `--campaign NAME` the run joins a persistent ledger: each
//!   run is a new round, and every reviewer receives the campaign's prior findings as a
//!   labelled data artifact.
//! - `ledger` prints a campaign's findings, one per line, machine-readably.
//! - `resolve` records the operator's disposition of one finding (fixed, wontfix, ...) in the
//!   campaign's ledger — the step between fixing and the round that verifies the fix.
//! - `group` and `ungroup` append reversible adjudication between duplicate Findings without
//!   erasing either identity, Report history, or verification obligation.
//! - `tui` drafts an explicit configuration patch for the pipeline's existing reviewer packages
//!   and launches the same pinned-authority `run` path from an alternate-screen interface.
//!
//! Nothing here mutates any repository. A run reads a repo and writes its own state
//! directory; `resolve` writes only that state; publishing results anywhere is a human's
//! explicit action.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use review_attempt::{Budget, BudgetLedger, Scope};
use review_core::{
    EventType, RunFailureReasonV2, RunFailureReasonV3, RunReportPayloadV2, RunReportPayloadV3,
    RunReportPayloadV4, RunReportPayloadV5, RunVerdictV2, RunVerdictV3, Severity,
};
use review_graph::NodeOutcome;
use review_pipeline::{Kernel, RoundAuthority, RunVerdict};
use review_runner::ReviewerAdapter;
use review_source_git::Repo;
use review_store::{Cas, EventStore, Ingest, Ledger, LedgerProjection, Status, Verdict};
use sha2::{Digest, Sha256};

mod authority;
mod caches;
mod cli;
mod config;
mod onboard;
mod project;
mod providers;
mod selfmgmt;
mod task;
mod topics;
mod tui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CampaignMode {
    Light,
    Heavy,
}

impl CampaignMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Heavy => "heavy",
        }
    }
}

#[derive(Clone)]
struct Options {
    repo: PathBuf,
    pipeline: PathBuf,
    /// `--pipeline` was given: routing never overrides an explicit selection.
    pipeline_explicit: bool,
    state: Option<PathBuf>,
    campaign: Option<String>,
    focus: Option<String>,
    policy_rev: Option<String>,
    base: Option<String>,
    candidate: Option<String>,
    /// Compatibility alias: expands to policy_rev + base for a new diff Campaign.
    authority: Option<String>,
    uncommitted: bool,
    restart_round: bool,
    mode: CampaignMode,
    timeout: Option<Duration>,
    git_timeout: Option<Duration>,
    provider_bindings: BTreeMap<String, String>,
    provider_resumes: BTreeMap<String, u64>,
    json: bool,
    /// `af review render`: the Worker whose exact input to compose.
    node: Option<String>,
}

impl Options {
    /// Resolve state once for both execution and presentation. Relative paths use the process
    /// working directory, preserving the CLI's historical meaning, and repository-contained
    /// state is confined to the review tree's `runs` directory.
    fn resolved_state_dir(&self) -> Result<PathBuf, String> {
        let requested = match (&self.state, &self.campaign) {
            (Some(state), _) => state.clone(),
            (None, Some(campaign)) => default_campaign_state(campaign)?,
            (None, None) => default_local_state(&self.repo)?,
        };
        let state = resolve_filesystem_path(&requested)?;
        if let Some(campaign) = &self.campaign
            && self.state.is_some()
        {
            validate_campaign_for_explicit_state(campaign, &state)?;
        }
        let repository = std::fs::canonicalize(&self.repo)
            .map_err(|error| format!("opening repository {}: {error}", self.repo.display()))?;
        if state.starts_with(&repository) {
            return Err(format!(
                "state {} is inside the repository; af state must live under XDG state or an explicit external --state directory",
                state.display()
            ));
        }
        Ok(state)
    }
}

fn xdg_state_root() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("XDG_STATE_HOME") {
        let configured = PathBuf::from(configured);
        if !configured.is_absolute() {
            return Err("XDG_STATE_HOME must be absolute".to_string());
        }
        return Ok(configured);
    }
    let user_home =
        std::env::var_os("HOME").ok_or("HOME is not set and XDG_STATE_HOME is absent")?;
    let user_home = PathBuf::from(user_home);
    if !user_home.is_absolute() {
        return Err("HOME must be absolute".to_string());
    }
    Ok(user_home.join(".local/state"))
}

fn default_campaign_state(campaign: &str) -> Result<PathBuf, String> {
    campaign_state_beneath(&xdg_state_root()?.join("af/review/campaigns"), campaign)
}

fn campaign_id(campaign: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"af/campaign-id@1\0");
    digest.update(campaign.as_bytes());
    let digest = digest.finalize();
    format!("c-{digest:x}")
}

fn campaign_state_beneath(root: &Path, campaign: &str) -> Result<PathBuf, String> {
    validate_legacy_campaign_name(campaign)?;
    let root = resolve_filesystem_path(root)?;
    let encoded = root.join(campaign_id(campaign));
    let legacy = root.join(campaign);
    let encoded_exists = encoded.exists();
    let legacy_belongs_to_campaign =
        legacy_campaign_state_matches(&legacy, campaign).map_err(|error| {
            format!(
                "legacy Campaign state {} blocks resolution of {campaign:?}: {error}",
                legacy.display()
            )
        })?;
    if encoded_exists && legacy_belongs_to_campaign {
        return Err(format!(
            "campaign {campaign:?} has both encoded and legacy state beneath {}; remove the ambiguity before continuing",
            root.display()
        ));
    }
    if !legacy_belongs_to_campaign {
        validate_campaign_name(campaign)?;
    }
    let selected = if legacy_belongs_to_campaign {
        legacy
    } else {
        encoded
    };
    let selected = resolve_filesystem_path(&selected)?;
    if !selected.starts_with(&root) {
        return Err(format!(
            "campaign state {} escapes configured review-state root {}",
            selected.display(),
            root.display()
        ));
    }
    Ok(selected)
}

fn default_local_state(repository: &Path) -> Result<PathBuf, String> {
    let repository = std::fs::canonicalize(repository)
        .map_err(|error| format!("opening repository {}: {error}", repository.display()))?;
    let identity = Sha256::digest(repository.as_os_str().as_encoded_bytes());
    Ok(xdg_state_root()?
        .join("af/review/local")
        .join(&format!("{identity:x}")[..16]))
}

fn validate_campaign_name(campaign: &str) -> Result<(), String> {
    validate_legacy_campaign_name(campaign)?;
    if campaign.trim() != campaign
        || campaign
            .chars()
            .any(|character| matches!(character, '/' | '\\') || character.is_control())
        || is_campaign_id(campaign)
    {
        return Err(format!(
            "campaign name {campaign:?} must be a trimmed human label without separators, control characters, traversal forms, or the reserved opaque Campaign ID shape"
        ));
    }
    Ok(())
}

fn validate_legacy_campaign_name(campaign: &str) -> Result<(), String> {
    let mut components = Path::new(campaign).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(format!(
            "campaign name {campaign:?} must be one safe path component"
        ));
    }
    Ok(())
}

fn is_campaign_id(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("c-")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn campaign_run_ids(state: &Path) -> Result<Vec<String>, String> {
    let database = state.join("events.sqlite");
    if !database.is_file() {
        return Ok(Vec::new());
    }
    EventStore::open_read_only(&database)
        .map_err(|error| {
            format!(
                "reading Campaign event store {}: {error}",
                database.display()
            )
        })?
        .run_ids()
        .map_err(|error| {
            format!(
                "reading Campaign event store {}: {error}",
                database.display()
            )
        })
        .map(|run_ids| {
            run_ids
                .into_iter()
                .filter(|run_id| run_id.starts_with("campaign-"))
                .collect()
        })
}

fn legacy_campaign_state_matches(state: &Path, campaign: &str) -> Result<bool, String> {
    Ok(campaign_run_ids(state)?.as_slice() == [campaign_run_id(campaign)])
}

fn validate_campaign_for_explicit_state(campaign: &str, state: &Path) -> Result<(), String> {
    if let Err(validation_error) = validate_campaign_name(campaign) {
        if campaign_run_ids(state)?
            .iter()
            .any(|run_id| run_id == &campaign_run_id(campaign))
        {
            validate_legacy_campaign_name(campaign)
        } else {
            Err(validation_error)
        }
    } else {
        Ok(())
    }
}

fn resolve_filesystem_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("reading current directory: {error}"))?
            .join(path)
    };
    let absolute = normalize_absolute(&absolute)?;
    let mut existing = absolute.clone();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| format!("path {} has no existing ancestor", absolute.display()))?
            .to_os_string();
        suffix.push(name);
        existing
            .pop()
            .then_some(())
            .ok_or_else(|| format!("path {} has no existing ancestor", absolute.display()))?;
    }
    let mut resolved = std::fs::canonicalize(&existing)
        .map_err(|error| format!("opening {}: {error}", existing.display()))?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    normalize_absolute(&resolved)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_) => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "path {} escapes its filesystem root",
                        path.display()
                    ));
                }
            }
        }
    }
    if !normalized.is_absolute() {
        return Err(format!(
            "path {} did not resolve absolutely",
            path.display()
        ));
    }
    Ok(normalized)
}

struct LedgerOptions {
    state: Option<PathBuf>,
    campaign: String,
    long: bool,
}

struct ShowOptions {
    state: Option<PathBuf>,
    campaign: String,
    key: String,
}

struct ExportOptions {
    state: Option<PathBuf>,
    campaign: String,
    proposal_id: Option<String>,
    finding_id: Option<String>,
    allow_stale: bool,
}

struct ReportOptions {
    state: Option<PathBuf>,
    campaign: String,
    format: ReportFormat,
}

struct CampaignsOptions {
    state_root: Option<PathBuf>,
    format: CampaignsFormat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CampaignsFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReportFormat {
    Markdown,
    Text,
    Json,
}

struct ResolveOptions {
    state: Option<PathBuf>,
    campaign: String,
    key: String,
    status: String,
    actor: Option<String>,
    policy_revision: String,
    reason: String,
    evidence_ids: Vec<String>,
    max_accepted_severity: Option<Severity>,
    tracking_reference: Option<String>,
    expires_at_policy_time: Option<u64>,
}

struct AttestChangeOptions {
    state: Option<PathBuf>,
    campaign: String,
    finding_id: String,
    actor: Option<String>,
    reason: String,
    regions: Vec<review_core::ChangedRegionV1>,
    evidence_ids: Vec<String>,
}

struct VerifyFixOptions {
    state: Option<PathBuf>,
    campaign: String,
    finding_id: String,
    attestation_id: String,
    verifier: Option<String>,
    policy_revision: String,
    reason: String,
    positive: bool,
    evidence_ids: Vec<String>,
}

struct ChallengeResolutionOptions {
    state: Option<PathBuf>,
    campaign: String,
    finding_id: String,
    kind: review_core::ResolutionChallengeKind,
    actor: Option<String>,
    reason: String,
    evidence_ids: Vec<String>,
}

struct PolicyTimeOptions {
    state: Option<PathBuf>,
    campaign: String,
    tick: u64,
    actor: Option<String>,
    reason: String,
}

struct GroupOptions {
    state: Option<PathBuf>,
    campaign: String,
    from: String,
    into: String,
}

struct EvidenceAddOptions {
    state: Option<PathBuf>,
    campaign: String,
    demand_id: String,
    file: PathBuf,
    actor: Option<String>,
}

struct EvidenceSatisfyOptions {
    state: Option<PathBuf>,
    campaign: String,
    demand_id: String,
    evidence_id: String,
    policy_revision: String,
    reason: String,
    actor: Option<String>,
    admit_reuse: bool,
}

struct DemandWaiveOptions {
    state: Option<PathBuf>,
    campaign: String,
    demand_id: String,
    actor: Option<String>,
    policy_revision: String,
    reason: String,
}

fn campaign_run_id(campaign: &str) -> String {
    format!("campaign-{campaign}")
}

fn campaign_state(state: &Option<PathBuf>, campaign: &str) -> Result<PathBuf, String> {
    match state {
        Some(state) => {
            let state = resolve_filesystem_path(state)?;
            validate_campaign_for_explicit_state(campaign, &state)?;
            Ok(state)
        }
        None => default_campaign_state(campaign),
    }
}

fn parse_changed_region(value: &str) -> Result<review_core::ChangedRegionV1, String> {
    let Some((path, lines)) = value.rsplit_once(':') else {
        return Ok(review_core::ChangedRegionV1 {
            path: value.into(),
            start_line: None,
            end_line: None,
        });
    };
    let Some((start, end)) = lines.split_once('-') else {
        return Err(format!("--region {value}: expected PATH or PATH:START-END"));
    };
    let parse = |text: &str| {
        text.parse::<u32>()
            .map_err(|_| format!("--region {value}: line numbers must be integers"))
    };
    Ok(review_core::ChangedRegionV1 {
        path: path.into(),
        start_line: Some(parse(start)?),
        end_line: Some(parse(end)?),
    })
}

/// A usage error clap could not express: print only the failing command's usage and exit 2.
fn usage_error(command: &str, message: &str) -> ! {
    eprintln!("error: {message}\n  try: af {command} --help");
    std::process::exit(2);
}

fn run_options(args: cli::RunArgs, command: &str) -> Options {
    let mut provider_bindings = BTreeMap::new();
    for binding in &args.provider {
        let Some((node, provider)) = binding.split_once('=') else {
            usage_error(
                command,
                &format!("--provider {binding}: expected NODE=PROVIDER_ID"),
            );
        };
        if node.is_empty() || provider.is_empty() {
            usage_error(
                command,
                &format!("--provider {binding}: NODE and PROVIDER_ID must both be present"),
            );
        }
        if provider_bindings
            .insert(node.to_string(), provider.to_string())
            .is_some()
        {
            usage_error(
                command,
                &format!("--provider {binding}: node `{node}` is bound twice"),
            );
        }
    }
    let mut provider_resumes = BTreeMap::new();
    for token in &args.resume_provider {
        let Some((operation, epoch)) = token.rsplit_once(':') else {
            usage_error(
                command,
                &format!("--resume-provider {token}: expected OPERATION_ID:EPOCH"),
            );
        };
        let epoch = epoch.parse::<u64>().unwrap_or(0);
        if operation.len() != 26
            || !operation
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            || epoch == 0
        {
            usage_error(
                command,
                &format!(
                    "--resume-provider {token}: OPERATION_ID is 26 lowercase alphanumerics and EPOCH is a positive integer"
                ),
            );
        }
        if provider_resumes
            .insert(operation.to_string(), epoch)
            .is_some()
        {
            usage_error(
                command,
                &format!("--resume-provider {token}: operation resumed twice"),
            );
        }
    }
    Options {
        repo: args.repo,
        // Routing never overrides an explicit selection; the default is the project's.
        pipeline_explicit: args.pipeline.is_some(),
        pipeline: args
            .pipeline
            .unwrap_or_else(|| PathBuf::from(cli::REVIEW_PIPELINE)),
        state: args.state,
        campaign: args.campaign,
        focus: args.focus,
        policy_rev: args.policy_rev,
        base: args.base,
        candidate: args.candidate,
        authority: args.authority,
        uncommitted: args.uncommitted,
        restart_round: args.restart_round,
        node: args.node,
        mode: if args.heavy {
            CampaignMode::Heavy
        } else {
            CampaignMode::Light
        },
        timeout: args.timeout_secs.map(Duration::from_secs),
        git_timeout: args.git_timeout_secs.map(Duration::from_secs),
        provider_bindings,
        provider_resumes,
        json: args.json,
    }
}

fn severity_of(arg: cli::SeverityArg) -> Severity {
    match arg {
        cli::SeverityArg::Minor => Severity::Minor,
        cli::SeverityArg::Major => Severity::Major,
        cli::SeverityArg::Blocker => Severity::Blocker,
    }
}

fn challenge_kind_of(arg: cli::ChallengeKindArg) -> review_core::ResolutionChallengeKind {
    match arg {
        cli::ChallengeKindArg::NewEvidence => review_core::ResolutionChallengeKind::NewEvidence,
        cli::ChallengeKindArg::HigherSeverity => {
            review_core::ResolutionChallengeKind::HigherSeverity
        }
        cli::ChallengeKindArg::OutsideScope => review_core::ResolutionChallengeKind::OutsideScope,
        cli::ChallengeKindArg::Expired => review_core::ResolutionChallengeKind::Expired,
    }
}

/// Campaign labels under the default state root, for shell completion. Bounded: one directory
/// listing, no Store beyond the manifest each campaign already exposes.
pub(crate) fn campaign_names_for_completion() -> Vec<String> {
    let Ok(root) = xdg_state_root() else {
        return Vec::new();
    };
    enumerate_campaigns(&root)
        .map(|enumeration| {
            enumeration
                .campaigns
                .into_iter()
                .map(|campaign| campaign.label)
                .filter(|label| !label.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn review_command(namespace: cli::ReviewNamespace) -> Result<i32, String> {
    use cli::ReviewCommand as R;
    let command = match namespace.command {
        Some(command) => command,
        None => R::Run(namespace.run),
    };
    match command {
        R::Run(args) => {
            init_review_workers();
            let verdict = run(&run_options(args, "review run"))?;
            Ok(match verdict {
                RunVerdict::Pass => 0,
                RunVerdict::Fail(_) => 3,
                RunVerdict::Incomplete { .. } => 4,
            })
        }
        R::Plan(args) => print_plan(&run_options(args, "review plan")).map(|()| 0),
        R::Render(args) => print_render(&run_options(args, "review render")).map(|()| 0),
        R::Tui(args) => {
            init_review_workers();
            tui::launch(run_options(args, "review tui")).map(|()| 0)
        }
        R::Ledger { selector, long } => print_ledger(&LedgerOptions {
            state: selector.state,
            campaign: selector.campaign,
            long,
        })
        .map(|()| 0),
        R::Show { selector, key } => show(&ShowOptions {
            state: selector.state,
            campaign: selector.campaign,
            key,
        })
        .map(|()| 0),
        R::Export {
            selector,
            proposal,
            finding,
            allow_stale,
        } => export_proposal(&ExportOptions {
            state: selector.state,
            campaign: selector.campaign,
            proposal_id: proposal,
            finding_id: finding,
            allow_stale,
        })
        .map(|()| 0),
        R::Report { selector, format } => print_report(&ReportOptions {
            state: selector.state,
            campaign: selector.campaign,
            format: match format {
                cli::ReportFormatArg::Md => ReportFormat::Markdown,
                cli::ReportFormatArg::Text => ReportFormat::Text,
                cli::ReportFormatArg::Json => ReportFormat::Json,
            },
        })
        .map(|()| 0),
        R::Campaigns { state_root, format } => print_campaigns(&CampaignsOptions {
            state_root,
            format: match format {
                cli::ListFormatArg::Text => CampaignsFormat::Text,
                cli::ListFormatArg::Json => CampaignsFormat::Json,
            },
        })
        .map(|()| 0),
        R::Resolve {
            selector,
            key,
            status,
            policy,
            reason,
            actor,
            max_severity,
            tracking,
            expires_at_policy_time,
        } => resolve(&ResolveOptions {
            state: selector.state,
            campaign: selector.campaign,
            key,
            status,
            actor: actor.actor,
            policy_revision: policy,
            reason,
            evidence_ids: actor.evidence,
            max_accepted_severity: max_severity.map(severity_of),
            tracking_reference: tracking,
            expires_at_policy_time,
        })
        .map(|()| 0),
        R::AttestChange {
            selector,
            finding,
            region,
            reason,
            actor,
        } => {
            let regions = region
                .iter()
                .map(|value| parse_changed_region(value))
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_else(|message| usage_error("review attest-change", &message));
            attest_change(&AttestChangeOptions {
                state: selector.state,
                campaign: selector.campaign,
                finding_id: finding,
                actor: actor.actor,
                reason,
                regions,
                evidence_ids: actor.evidence,
            })
            .map(|()| 0)
        }
        R::VerifyFix {
            selector,
            finding,
            attestation,
            policy,
            reason,
            positive,
            negative: _,
            verifier,
            evidence,
        } => verify_fix(&VerifyFixOptions {
            state: selector.state,
            campaign: selector.campaign,
            finding_id: finding,
            attestation_id: attestation,
            verifier,
            policy_revision: policy,
            reason,
            positive,
            evidence_ids: evidence,
        })
        .map(|()| 0),
        R::ChallengeResolution {
            selector,
            finding,
            kind,
            reason,
            actor,
        } => challenge_resolution(&ChallengeResolutionOptions {
            state: selector.state,
            campaign: selector.campaign,
            finding_id: finding,
            kind: challenge_kind_of(kind),
            actor: actor.actor,
            reason,
            evidence_ids: actor.evidence,
        })
        .map(|()| 0),
        R::PolicyTime { command } => match command {
            cli::PolicyTimeCommand::Advance {
                selector,
                tick,
                reason,
                actor,
            } => advance_policy_time(&PolicyTimeOptions {
                state: selector.state,
                campaign: selector.campaign,
                tick,
                actor,
                reason,
            })
            .map(|()| 0),
        },
        R::Group {
            selector,
            from,
            into,
        } => group(
            &GroupOptions {
                state: selector.state,
                campaign: selector.campaign,
                from,
                into,
            },
            false,
        )
        .map(|()| 0),
        R::Ungroup {
            selector,
            from,
            into,
        } => group(
            &GroupOptions {
                state: selector.state,
                campaign: selector.campaign,
                from,
                into,
            },
            true,
        )
        .map(|()| 0),
        R::Evidence { command } => match command {
            cli::EvidenceCommand::Add {
                selector,
                demand,
                file,
                actor,
            } => add_evidence(&EvidenceAddOptions {
                state: selector.state,
                campaign: selector.campaign,
                demand_id: demand,
                file,
                actor,
            })
            .map(|()| 0),
            cli::EvidenceCommand::Satisfy {
                selector,
                demand,
                evidence,
                policy,
                reason,
                admit_reuse,
                actor,
            } => satisfy_evidence(&EvidenceSatisfyOptions {
                state: selector.state,
                campaign: selector.campaign,
                demand_id: demand,
                evidence_id: evidence,
                policy_revision: policy,
                reason,
                actor,
                admit_reuse,
            })
            .map(|()| 0),
        },
        R::Demand { command } => match command {
            cli::DemandCommand::Waive {
                selector,
                demand,
                policy,
                reason,
                actor,
            } => waive_demand(&DemandWaiveOptions {
                state: selector.state,
                campaign: selector.campaign,
                demand_id: demand,
                actor,
                policy_revision: policy,
                reason,
            })
            .map(|()| 0),
        },
    }
}

fn help_command(words: &[String]) -> Result<i32, String> {
    use clap::CommandFactory as _;
    let mut root = cli::Af::command();
    if words.is_empty() {
        root.print_long_help().map_err(|error| error.to_string())?;
        return Ok(0);
    }
    if words.len() == 1
        && let Some((topic, about, text)) = topics::find(&words[0])
    {
        println!("af help {topic} — {about}\n\n{text}");
        return Ok(0);
    }
    let mut current = &mut root;
    for word in words {
        match current.find_subcommand_mut(word) {
            Some(sub) => current = sub,
            None => {
                let topics: Vec<&str> = topics::TOPICS.iter().map(|(name, _, _)| *name).collect();
                eprintln!(
                    "error: `{}` is neither a topic nor a command\n  topics: {}\n  try: af --help",
                    words.join(" "),
                    topics.join(", ")
                );
                return Ok(2);
            }
        }
    }
    let name = current.get_name().to_string();
    let path = format!("af {}", words.join(" "));
    current
        .clone()
        .name(name)
        .bin_name(path)
        .print_long_help()
        .map_err(|error| error.to_string())?;
    Ok(0)
}

fn main() {
    use clap::{CommandFactory as _, Parser as _};
    let argv: Vec<String> = std::env::args().collect();
    clap_complete::CompleteEnv::with_factory(cli::Af::command).complete();
    selfmgmt::maybe_dispatch(&argv);
    let parsed = cli::Af::parse();
    if parsed.version {
        if let Err(error) = selfmgmt::print_version(parsed.json) {
            eprintln!("af: {error}");
            std::process::exit(1);
        }
        return;
    }
    let Some(command) = parsed.command else {
        let _ = cli::Af::command().print_help();
        std::process::exit(2);
    };
    let (prefix, outcome): (&str, Result<i32, String>) = match command {
        cli::Command::Review(namespace) => ("af review", review_command(namespace)),
        cli::Command::Provider { command } => (
            "af provider",
            match command {
                cli::ProviderCommand::Status => {
                    providers::print_status();
                    Ok(0)
                }
                cli::ProviderCommand::Doctor(args) => {
                    provider_doctor(&run_options(args, "provider doctor")).map(|()| 0)
                }
            },
        ),
        cli::Command::Onboard(args) => ("af onboard", onboard::run_cli(args).map(|()| 0)),
        cli::Command::Task { command } => (
            "af task",
            match command {
                cli::TaskCommand::Start {
                    kind: _,
                    goal,
                    repo,
                    pipeline,
                    state,
                    authority,
                    uncommitted,
                    timeout_secs,
                    json,
                } => task::options_from_cli(
                    goal,
                    repo,
                    pipeline,
                    state,
                    authority,
                    uncommitted,
                    timeout_secs,
                    json,
                )
                .and_then(|options| {
                    init_review_workers();
                    task::start(options)
                })
                .map(|verified| if verified { 0 } else { 3 }),
                cli::TaskCommand::Deliver {
                    task_id,
                    repo,
                    branch,
                    worktree,
                    confirm,
                    state,
                    json,
                } => task::delivery_from_cli(task_id, repo, branch, worktree, confirm, state, json)
                    .and_then(task::deliver)
                    .map(|()| 0),
                cli::TaskCommand::List { inspect } => {
                    task::inspect_from_cli(None, inspect.repo, inspect.state, inspect.json)
                        .and_then(task::list)
                        .map(|()| 0)
                }
                cli::TaskCommand::Show { task_id, inspect } => {
                    task::inspect_from_cli(Some(task_id), inspect.repo, inspect.state, inspect.json)
                        .and_then(task::show)
                        .map(|()| 0)
                }
            },
        ),
        cli::Command::Config { command } => (
            "af config",
            match command {
                cli::ConfigCommand::Show { origin, json, repo } => {
                    config::show(repo.as_deref(), origin, json).map(|()| 0)
                }
                cli::ConfigCommand::Edit { layer, repo } => {
                    config::edit(layer, repo.as_deref()).map(|()| 0)
                }
                cli::ConfigCommand::Paths { repo } => config::paths(repo.as_deref()).map(|()| 0),
            },
        ),
        cli::Command::SelfCmd { command } => (
            "af self",
            match command {
                cli::SelfCommand::Status { json } => selfmgmt::status(json).map(|()| 0),
                cli::SelfCommand::Update { check, version, rc } => {
                    selfmgmt::update(check, version, rc).map(|()| 0)
                }
                cli::SelfCommand::Rollback => selfmgmt::rollback().map(|()| 0),
                cli::SelfCommand::Install { version } => {
                    selfmgmt::install_command(&version).map(|()| 0)
                }
                cli::SelfCommand::Remove { version } => selfmgmt::remove(&version).map(|()| 0),
                cli::SelfCommand::Prune => selfmgmt::prune().map(|()| 0),
                cli::SelfCommand::SetupShell { shell, write } => {
                    selfmgmt::setup_shell(shell, write).map(|()| 0)
                }
                cli::SelfCommand::Uninstall { purge } => selfmgmt::uninstall(purge).map(|()| 0),
                cli::SelfCommand::Man { out_dir } => selfmgmt::man(&out_dir).map(|()| 0),
                cli::SelfCommand::RefreshCheck => selfmgmt::refresh_check().map(|()| 0),
            },
        ),
        cli::Command::Completions { shell } => (
            "af completions",
            selfmgmt::completion_script(shell).map(|script| {
                print!("{script}");
                0
            }),
        ),
        cli::Command::Help { words } => ("af help", help_command(&words)),
    };
    let code = match outcome {
        Ok(code) => code,
        Err(error) => {
            if argv.iter().skip(1).any(|word| word == "--json") {
                let document = serde_json::json!({
                    "schema": "af/error@1",
                    "command": prefix,
                    "error": error,
                    "exit_code": 1,
                });
                println!("{document}");
            }
            eprintln!("{prefix}: {error}");
            1
        }
    };
    selfmgmt::after_command(&argv);
    if code != 0 {
        std::process::exit(code);
    }
}

fn print_render(options: &Options) -> Result<(), String> {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|error| error.to_string())?;
    let repository = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    let git_home = scratch.path().join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(repository, git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let view = authority::render(options, &cas, &repo)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string(&view).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    // The header goes to stderr and the exact bytes to stdout, so `> prompt.md` is exact.
    eprintln!("review render (token-free; no Campaign state)");
    eprintln!("node      {} ({})", view.node, view.runner);
    if let Some(package) = &view.package {
        eprintln!(
            "package   {} {} {}",
            package["name"].as_str().unwrap_or("?"),
            package["version"].as_str().unwrap_or("?"),
            package["digest"].as_str().unwrap_or("?")
        );
    }
    eprintln!(
        "transport {}; {} bytes; about {} tokens{}",
        match view.transport {
            review_runner::InputTransport::Prompt => "prompt on stdin",
            review_runner::InputTransport::Json => "typed JSON on stdin",
        },
        view.bytes,
        view.estimated_tokens,
        match (view.cap_tokens, view.fits) {
            (Some(cap), Some(true)) => format!("; fits its {cap}-token Attempt cap"),
            (Some(cap), Some(false)) => format!("; EXCEEDS its {cap}-token Attempt cap"),
            _ => String::new(),
        }
    );
    for entry in &view.manifest.entries {
        eprintln!(
            "context   {} {} bytes ({})",
            entry.name, entry.rendered_bytes, entry.required_by
        );
    }
    for item in &view.not_rendered {
        eprintln!("omitted   {item}");
    }
    eprintln!("effects   no state, Gates, Provider calls, Workers, or token spend");
    let mut stdout = std::io::stdout().lock();
    std::io::Write::write_all(&mut stdout, &view.raw).map_err(|error| error.to_string())?;
    std::io::Write::flush(&mut stdout).map_err(|error| error.to_string())?;
    Ok(())
}

fn print_plan(options: &Options) -> Result<(), String> {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|error| error.to_string())?;
    let repository = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    let git_home = scratch.path().join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(repository, git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let plan = authority::plan(options, &cas, &repo)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string(&plan).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    let selectors = &plan["selectors"];
    let resolved = &plan["resolved"];
    println!("review plan (token-free; no Campaign state)");
    if let Some(authority) = selectors["compatibility_authority"].as_str() {
        if plan["subject"]["kind"].as_str() == Some("diff") {
            println!(
                "compat   --authority {authority} => --policy-rev {authority} --base {authority}"
            );
        } else {
            println!("compat   --authority {authority} => --policy-rev {authority}");
        }
    }
    println!(
        "policy   {} => {}",
        selectors["policy_rev"].as_str().unwrap_or("?"),
        resolved["policy_snapshot_id"].as_str().unwrap_or("?")
    );
    if let Some(base) = selectors["base"].as_str() {
        println!(
            "base     {base} => {}",
            resolved["base_snapshot_id"].as_str().unwrap_or("?")
        );
    }
    println!(
        "candidate {} => {}",
        selectors["candidate"].as_str().unwrap_or("?"),
        resolved["candidate_snapshot_id"].as_str().unwrap_or("?")
    );
    println!(
        "subject  {} ({} paths, {} patch bytes{})",
        plan["subject"]["kind"].as_str().unwrap_or("?"),
        plan["subject"]["changed_paths"]
            .as_array()
            .map_or(0, Vec::len),
        plan["subject"]["patch_bytes"].as_u64().unwrap_or(0),
        if plan["subject"]["empty"].as_bool() == Some(true) {
            "; EMPTY — run will refuse"
        } else {
            ""
        }
    );
    for path in plan["subject"]["changed_paths"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!("  change  {}", path.as_str().unwrap_or("?"));
    }
    if let Some(route) = plan["route"].as_object() {
        println!(
            "route    {} => {}{}{}",
            route
                .get("policy")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
            route
                .get("pipeline_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
            route
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(|name| format!(" ({name})"))
                .unwrap_or_default(),
            route
                .get("replaced")
                .and_then(serde_json::Value::as_str)
                .map(|replaced| format!("; replaced {replaced}: its Worker input exceeded the cap"))
                .unwrap_or_default()
        );
    }
    println!("topology");
    for node in plan["pipeline"]["topology"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!(
            "  node {} [{}]",
            node["id"].as_str().unwrap_or("?"),
            node["kind"].as_str().unwrap_or("?")
        );
    }
    for edge in plan["pipeline"]["topology"]["edges"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!(
            "  {}.{} -> {}.{}",
            edge["from"]["node"].as_str().unwrap_or("?"),
            edge["from"]["port"].as_str().unwrap_or("?"),
            edge["to"]["node"].as_str().unwrap_or("?"),
            edge["to"]["port"].as_str().unwrap_or("?")
        );
    }
    for gate in plan["pipeline"]["gates"].as_array().into_iter().flatten() {
        println!(
            "gate     {} ({})",
            gate["name"].as_str().unwrap_or("?"),
            if gate["required"].as_bool() == Some(true) {
                "required"
            } else {
                "optional"
            }
        );
    }
    if let Some(budgets) = plan["pipeline"]["budgets"].as_object() {
        println!(
            "budgets  {} tokens/Attempt; {} tokens/Round",
            budgets
                .get("attempt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            budgets
                .get("run")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        );
    }
    if let Some(reservations) = plan["pipeline"]["reservations"]
        .as_array()
        .filter(|reservations| !reservations.is_empty())
    {
        let parts = reservations
            .iter()
            .map(|reservation| {
                format!(
                    "{} {} ({})",
                    reservation["node"].as_str().unwrap_or("?"),
                    reservation["tokens"].as_u64().unwrap_or(0),
                    reservation["source"].as_str().unwrap_or("?")
                )
            })
            .collect::<Vec<_>>();
        println!(
            "reserve  {}; max simultaneous {}",
            parts.join(" · "),
            plan["pipeline"]["max_simultaneous_reservation"]
                .as_u64()
                .unwrap_or(0)
        );
        for reservation in reservations {
            let Some(bytes) = reservation["input_bytes"].as_u64() else {
                continue;
            };
            println!(
                "input    {} {bytes} bytes (about {} tokens){}",
                reservation["node"].as_str().unwrap_or("?"),
                reservation["input_tokens"].as_u64().unwrap_or(0),
                if reservation["fits"].as_bool() == Some(false) {
                    " — EXCEEDS its Attempt cap; run will refuse"
                } else {
                    ""
                }
            );
        }
    }
    for provider in plan["providers"].as_array().into_iter().flatten() {
        println!(
            "provider {} -> {}",
            provider["node"].as_str().unwrap_or("?"),
            provider["binding"].as_str().unwrap_or("MISSING")
        );
    }
    println!("effects  no state, Gates, Provider calls, Workers, or token spend");
    Ok(())
}

fn run_progress(options: &Options, arguments: fmt::Arguments<'_>) {
    if options.json {
        eprintln!("{arguments}");
    } else {
        println!("{arguments}");
    }
}

fn init_review_workers() {
    let worker_limit = std::thread::available_parallelism()
        .map(|workers| workers.get())
        .unwrap_or(1);
    review_parallel::init_worker_limit(worker_limit)
        .expect("review worker executor is initialized once before execution");
}

fn open_campaign_store(state: &Path) -> Result<EventStore, String> {
    if !state.join("events.sqlite").exists() {
        return Err(format!(
            "no campaign state at {}; a campaign starts with `af review run --campaign`",
            state.display()
        ));
    }
    EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())
}

fn open_campaign_store_read_only(state: &Path) -> Result<EventStore, String> {
    if !state.join("events.sqlite").exists() {
        return Err(format!(
            "no campaign state at {}; a campaign starts with `af review run --campaign`",
            state.display()
        ));
    }
    EventStore::open_read_only(state.join("events.sqlite")).map_err(|error| error.to_string())
}

fn print_ledger(options: &LedgerOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let ledger = LedgerProjection::rebuild(&store, &cas, &campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?
        .into_ledger();
    let events = store
        .replay(&campaign_run_id(&options.campaign))
        .map_err(|error| error.to_string())?;
    if let Some(evidence) = latest_round_evidence(&events, &cas)?
        && evidence.ledger_was_not_produced()
    {
        eprintln!(
            "latest round Ledger: not produced because {}; showing the last gathered projection ({} admitted result(s) remain recorded, not gathered)",
            evidence.absence_reason(),
            evidence.available_node_results.len()
        );
    }
    print_scope_authority_warnings(&ledger);
    let findings = ledger.finding_views();
    for finding in &findings {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}:{}\t{}",
            finding.key,
            format!("{:?}", finding.severity).to_lowercase(),
            finding.status.as_str(),
            finding.convergence_scope_label(),
            finding
                .convergence_severity
                .map(|severity| format!("{severity:?}").to_lowercase())
                .unwrap_or_else(|| "-".to_string()),
            finding.file,
            finding.line.map_or("-".to_string(), |l| l.to_string()),
            finding.title
        );
        if !finding.aliases.is_empty() {
            print_indented("aliases", &finding.aliases.join(", "));
        }
        if options.long {
            print_indented("body", &finding.body);
            print_indented(
                "fix",
                finding
                    .fix
                    .as_deref()
                    .unwrap_or("(unavailable: artifact-less legacy import)"),
            );
            print_indented(
                "report scopes",
                &finding
                    .reports
                    .iter()
                    .map(|report| {
                        format!(
                            "{} round {}={} at {}:{}",
                            report.source,
                            report.round,
                            report.scope_label(),
                            report.file,
                            report.line.map_or("-".to_string(), |line| line.to_string())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
    }
    let open_required_demands = ledger
        .demand_views()
        .into_iter()
        .filter(|demand| {
            demand.requirement == review_core::DemandRequirement::Required
                && matches!(
                    demand.status,
                    review_core::DemandStatus::Open | review_core::DemandStatus::Stale
                )
        })
        .count();
    let summary = findings_summary(&findings);
    let wall = wall_span_ms(
        &store
            .attempt_wall(&campaign_run_id(&options.campaign))
            .map_err(|e| e.to_string())?,
    );
    eprintln!(
        "round {}; {} findings: {}; {} required demands open/stale{}",
        ledger.round,
        findings.len(),
        findings_summary_line(&summary),
        open_required_demands,
        wall.map(|ms| format!("; wall {}", human_duration(ms)))
            .unwrap_or_default()
    );
    Ok(())
}

fn print_indented(label: &str, value: &str) {
    let mut lines = value.lines();
    println!("  {label}: {}", lines.next().unwrap_or_default());
    for line in lines {
        println!("    {line}");
    }
}

struct CampaignProposal {
    id: String,
    proposal: review_core::PatchProposal,
}

fn campaign_proposals(
    store: &EventStore,
    cas: &Cas,
    run_id: &str,
) -> Result<Vec<CampaignProposal>, String> {
    let mut proposals = BTreeMap::new();
    for event in store
        .replay(run_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|event| event.event_type == EventType::ProposalAcceptedV1)
    {
        let accepted: review_core::ProposalAcceptedPayloadV1 =
            serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&accepted.proposal_artifact_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        review_store::validate_envelope(&envelope)?;
        if envelope.artifact_type != review_core::contract::PATCH_PROPOSAL_V1
            || envelope.artifact_id != accepted.proposal_id
        {
            return Err(format!(
                "accepted Proposal {} contradicts its artifact envelope",
                accepted.proposal_id
            ));
        }
        let proposal: review_core::PatchProposal =
            serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
        proposal.check_shape().map_err(str::to_string)?;
        cas.verify(&proposal.patch_artifact_id)
            .map_err(|error| error.to_string())?;
        let view = CampaignProposal {
            id: accepted.proposal_id.clone(),
            proposal,
        };
        if proposals.insert(accepted.proposal_id, view).is_some() {
            return Err("Campaign records one Proposal ID more than once".into());
        }
    }
    Ok(proposals.into_values().collect())
}

fn current_campaign_head(store: &EventStore, cas: &Cas, run_id: &str) -> Result<String, String> {
    let started = store
        .replay(run_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .rev()
        .find(|event| event.event_type == EventType::RoundStartedV1)
        .ok_or_else(|| "Campaign has no started Round".to_string())?;
    let payload: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(started.payload).map_err(|error| error.to_string())?;
    review_store::resolve_subject(cas, &payload.subject_id)
        .map(|resolved| resolved.subject.head_snapshot_id)
        .map_err(|error| error.to_string())
}

fn finding_claim_ids(
    finding: &review_store::Finding,
    cas: &Cas,
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let mut findings = BTreeSet::from([finding.key.clone()]);
    findings.extend(finding.aliases.iter().cloned());
    let mut reports = BTreeSet::new();
    for report in &finding.reports {
        if report.report_id.is_empty() {
            continue;
        }
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&report.report_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        review_store::validate_envelope(&envelope)?;
        if envelope.artifact_type == review_core::contract::FINDING_REPORT_V1 {
            reports.insert(envelope.artifact_id);
        }
    }
    Ok((findings, reports))
}

fn proposal_links_finding(
    proposal: &review_core::PatchProposal,
    finding_ids: &BTreeSet<String>,
    report_ids: &BTreeSet<String>,
) -> bool {
    proposal.finding_refs.iter().any(|claim| match claim.kind {
        review_core::ClaimRefKind::Finding => finding_ids.contains(&claim.id),
        review_core::ClaimRefKind::Report => report_ids.contains(&claim.id),
    })
}

fn export_proposal(options: &ExportOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let store = open_campaign_store_read_only(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|error| error.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let proposals = campaign_proposals(&store, &cas, &run_id)?;
    let head = current_campaign_head(&store, &cas, &run_id)?;
    let selected = if let Some(proposal_id) = &options.proposal_id {
        proposals
            .iter()
            .find(|proposal| &proposal.id == proposal_id)
            .ok_or_else(|| format!("no Proposal with ID {proposal_id}"))?
    } else {
        let finding_id = options
            .finding_id
            .as_deref()
            .expect("parser requires Finding");
        let ledger = LedgerProjection::rebuild(&store, &cas, &run_id)
            .map_err(|error| error.to_string())?
            .into_ledger();
        let finding = ledger
            .finding_view(finding_id)
            .ok_or_else(|| format!("no finding with key {finding_id}"))?;
        let (finding_ids, report_ids) = finding_claim_ids(&finding, &cas)?;
        let current = proposals
            .iter()
            .filter(|proposal| {
                proposal.proposal.base_snapshot_id == head
                    && proposal_links_finding(&proposal.proposal, &finding_ids, &report_ids)
            })
            .collect::<Vec<_>>();
        if current.len() != 1 {
            return Err(format!(
                "finding {finding_id} has {} current Proposals; export by exact Proposal ID",
                current.len()
            ));
        }
        current[0]
    };
    let stale = selected.proposal.base_snapshot_id != head;
    if stale && !options.allow_stale {
        return Err(format!(
            "Proposal {} is stale: base {} differs from current Campaign head {}; pass --allow-stale only for deliberate three-way application",
            selected.id, selected.proposal.base_snapshot_id, head
        ));
    }
    let patch = cas
        .get(&selected.proposal.patch_artifact_id)
        .map_err(|error| error.to_string())?;
    std::io::stdout()
        .lock()
        .write_all(&patch)
        .map_err(|error| error.to_string())
}

fn show(options: &ShowOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let ledger = LedgerProjection::rebuild(&store, &cas, &campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?
        .into_ledger();
    print_scope_authority_warnings(&ledger);
    let finding = ledger
        .finding_view(&options.key)
        .ok_or_else(|| format!("no finding with key {}", options.key))?;
    let run_id = campaign_run_id(&options.campaign);
    let head = current_campaign_head(&store, &cas, &run_id)?;
    let proposals = campaign_proposals(&store, &cas, &run_id)?;
    let (finding_ids, report_ids) = finding_claim_ids(&finding, &cas)?;

    println!("{} [{}]", finding.title, finding.key);
    if !finding.aliases.is_empty() {
        println!("aliases={}", finding.aliases.join(","));
    }
    println!(
        "severity={} effective_severity={} status={} scope={} location={}:{}",
        format!("{:?}", finding.severity).to_lowercase(),
        finding
            .convergence_severity
            .map(|severity| format!("{severity:?}").to_lowercase())
            .unwrap_or_else(|| "-".to_string()),
        finding.status.as_str(),
        finding.convergence_scope_label(),
        finding.file,
        finding
            .line
            .map_or("-".to_string(), |line| line.to_string())
    );
    for (index, attached) in finding.reports.iter().enumerate() {
        println!(
            "\nreport {}: reviewer={} round={} severity={} scope={} location={}:{} id={}",
            index + 1,
            attached.source,
            attached.round,
            format!("{:?}", attached.severity).to_lowercase(),
            attached.scope_label(),
            attached.file,
            attached
                .line
                .map_or("-".to_string(), |line| line.to_string()),
            if attached.report_id.is_empty() {
                "(unavailable: legacy import)"
            } else {
                &attached.report_id
            }
        );
        if attached.report_id.is_empty() {
            print_indented("body", &finding.body);
            print_indented("fix", "(unavailable: artifact-less legacy import)");
            println!(
                "  confidence: {}",
                finding
                    .confidence
                    .map_or("(unavailable)".to_string(), |value| value.to_string())
            );
        } else {
            let report = cas
                .get_json(&attached.report_id)
                .map_err(|e| format!("reading report {}: {e}", attached.report_id))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
            );
        }
    }

    println!("\nhistory:");
    for transition in &finding.history {
        println!(
            "  round {}: {:?}{}",
            transition.round,
            transition.kind,
            transition
                .note
                .as_deref()
                .map(|note| format!(" - {note}"))
                .unwrap_or_default()
        );
    }
    println!(
        "current note: {}",
        finding.current_note().unwrap_or("(none)")
    );
    println!("\nproposals:");
    let linked = proposals
        .iter()
        .filter(|proposal| proposal_links_finding(&proposal.proposal, &finding_ids, &report_ids))
        .collect::<Vec<_>>();
    if linked.is_empty() {
        println!("  (none)");
    } else {
        for proposal in linked {
            println!(
                "  {} base={} applicability={} paths={}",
                proposal.id,
                proposal.proposal.base_snapshot_id,
                if proposal.proposal.base_snapshot_id == head {
                    "current"
                } else {
                    "stale"
                },
                proposal.proposal.paths.join(",")
            );
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct CampaignListView {
    schema: &'static str,
    campaigns: Vec<CampaignView>,
    problems: Vec<CampaignProblemView>,
}

#[derive(serde::Serialize)]
struct CampaignProblemView {
    directory: String,
    reason: String,
}

struct CampaignEnumeration {
    campaigns: Vec<CampaignView>,
    problems: Vec<CampaignProblemView>,
}

#[derive(serde::Serialize)]
struct CampaignView {
    id: String,
    label: String,
    authority_snapshot_id: String,
    campaign_manifest_id: String,
    subject_kind: review_core::SubjectKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_subject_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_closed_round: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_closed_epoch: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict: Option<String>,
    rounds: Vec<ReportRoundView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wall_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    findings: Option<FindingsSummaryView>,
}

fn print_campaigns(options: &CampaignsOptions) -> Result<(), String> {
    let requested_root = match &options.state_root {
        Some(root) => root.clone(),
        None => xdg_state_root()?.join("af/review/campaigns"),
    };
    let root = resolve_filesystem_path(&requested_root)?;
    let enumeration = enumerate_campaigns(&root)?;
    let view = CampaignListView {
        schema: "af/review-campaigns@1",
        campaigns: enumeration.campaigns,
        problems: enumeration.problems,
    };
    match options.format {
        CampaignsFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&view).map_err(|error| error.to_string())?
        ),
        CampaignsFormat::Text => print_campaigns_text(&view),
    }
    Ok(())
}

fn enumerate_campaigns(root: &Path) -> Result<CampaignEnumeration, String> {
    let root = resolve_filesystem_path(root)?;
    if !root.exists() {
        return Ok(CampaignEnumeration {
            campaigns: Vec::new(),
            problems: Vec::new(),
        });
    }
    if !root.is_dir() {
        return Err(format!(
            "campaign state root {} is not a directory",
            root.display()
        ));
    }
    let mut paths = std::fs::read_dir(&root)
        .map_err(|error| format!("reading campaign state root {}: {error}", root.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("reading campaign state root {}: {error}", root.display()))?;
    paths.sort_by_key(std::fs::DirEntry::file_name);

    let mut campaigns = Vec::new();
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in paths {
        let directory_lossy = entry.file_name().to_string_lossy().into_owned();
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                problems.push(CampaignProblemView {
                    directory: directory_lossy,
                    reason: format!("reading entry metadata: {error}"),
                });
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            problems.push(CampaignProblemView {
                directory: directory_lossy,
                reason: "state-root entry is a symlink; Campaign enumeration does not follow it"
                    .to_string(),
            });
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        let database = entry.path().join("events.sqlite");
        let database_metadata = match std::fs::symlink_metadata(&database) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                problems.push(CampaignProblemView {
                    directory: directory_lossy,
                    reason: format!("reading events.sqlite metadata: {error}"),
                });
                continue;
            }
        };
        if database_metadata.file_type().is_symlink() || !database_metadata.is_file() {
            problems.push(CampaignProblemView {
                directory: directory_lossy,
                reason:
                    "events.sqlite is not a real regular file beneath the Campaign state directory"
                        .to_string(),
            });
            continue;
        }
        let state = match resolve_filesystem_path(&entry.path()) {
            Ok(state) => state,
            Err(reason) => {
                problems.push(CampaignProblemView {
                    directory: directory_lossy,
                    reason,
                });
                continue;
            }
        };
        if !state.starts_with(&root) {
            problems.push(CampaignProblemView {
                directory: directory_lossy,
                reason: format!(
                    "resolved state {} escapes configured root {}",
                    state.display(),
                    root.display()
                ),
            });
            continue;
        }
        let store = match open_campaign_store_read_only(&state) {
            Ok(store) => store,
            Err(reason) => {
                problems.push(CampaignProblemView {
                    directory: directory_lossy,
                    reason,
                });
                continue;
            }
        };
        let run_ids = match store.run_ids() {
            Ok(run_ids) => run_ids,
            Err(error) => {
                problems.push(CampaignProblemView {
                    directory: directory_lossy,
                    reason: error.to_string(),
                });
                continue;
            }
        }
        .into_iter()
        .filter(|run_id| run_id.starts_with("campaign-"))
        .collect::<Vec<_>>();
        if run_ids.len() != 1 {
            problems.push(CampaignProblemView {
                directory: directory_lossy,
                reason: format!(
                    "state contains {} campaign runs; each enumerated directory must contain exactly one",
                    run_ids.len()
                ),
            });
            continue;
        }
        let run_id = &run_ids[0];
        let label = run_id
            .strip_prefix("campaign-")
            .expect("campaign run prefix was filtered")
            .to_string();
        if let Err(reason) = validate_legacy_campaign_name(&label) {
            problems.push(CampaignProblemView {
                directory: directory_lossy,
                reason,
            });
            continue;
        }
        let id = campaign_id(&label);
        let directory = match entry.file_name().into_string() {
            Ok(directory) => directory,
            Err(_) => {
                problems.push(CampaignProblemView {
                    directory: directory_lossy,
                    reason: "Campaign state directory name is not valid UTF-8".to_string(),
                });
                continue;
            }
        };
        if directory != id && directory != label {
            problems.push(CampaignProblemView {
                directory,
                reason: format!(
                    "campaign {label:?} state directory must be its opaque ID `{id}` or legacy label"
                ),
            });
            continue;
        }
        if directory == label && root.join(&id).exists() {
            return Err(format!(
                "campaign {label:?} has both encoded and legacy state beneath {}; remove the ambiguity before continuing",
                root.display()
            ));
        }
        if directory == id {
            match legacy_campaign_state_matches(&root.join(&label), &label) {
                Ok(true) => {
                    return Err(format!(
                        "campaign {label:?} has both encoded and legacy state beneath {}; remove the ambiguity before continuing",
                        root.display()
                    ));
                }
                Ok(false) => {}
                Err(reason) => {
                    problems.push(CampaignProblemView {
                        directory,
                        reason: format!(
                            "legacy sibling state for campaign {label:?} blocks direct resolution: {reason}"
                        ),
                    });
                    continue;
                }
            }
        }
        let campaign = match read_campaign_view(&state, run_id, label.clone(), id.clone(), &store) {
            Ok(campaign) => campaign,
            Err(reason) => {
                problems.push(CampaignProblemView { directory, reason });
                continue;
            }
        };
        if !seen.insert(id.clone()) {
            return Err(format!(
                "campaign {label:?} is present in both encoded and legacy state directories beneath {}",
                root.display()
            ));
        }
        campaigns.push(campaign);
    }
    campaigns.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(CampaignEnumeration {
        campaigns,
        problems,
    })
}

fn read_campaign_view(
    state: &Path,
    run_id: &str,
    label: String,
    id: String,
    store: &EventStore,
) -> Result<CampaignView, String> {
    let opened = store
        .campaign_opened(run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("campaign {label:?} has no CampaignOpened event"))?;
    let opened: review_core::CampaignOpenedPayloadV1 = serde_json::from_value(opened.payload)
        .map_err(|error| format!("reading campaign {label:?} opening: {error}"))?;
    opened.validate()?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|error| error.to_string())?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&opened.campaign_manifest_id)
            .map_err(|error| format!("reading campaign {label:?} manifest: {error}"))?,
    )
    .map_err(|error| format!("reading campaign {label:?} manifest: {error}"))?;
    manifest.validate()?;
    if manifest.authority_snapshot_id != opened.authority_snapshot_id {
        return Err(format!(
            "campaign {label:?} opening and manifest disagree on authority Snapshot ID"
        ));
    }

    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    let mut last_subject_id = None;
    for event in events
        .iter()
        .filter(|event| event.event_type == EventType::RoundStartedV1)
    {
        let started: review_core::RoundStartedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        if started.campaign_manifest_id != opened.campaign_manifest_id {
            return Err(format!(
                "campaign {label:?} Round {} epoch {} refers to a different manifest",
                started.round, started.epoch
            ));
        }
        last_subject_id = Some(started.subject_id);
    }
    let round_authority = report_round_authority(&events)?;
    let reports = events
        .iter()
        .filter(|event| event.event_type.is_run_report())
        .collect::<Vec<_>>();
    let rounds = report_rounds(&reports, &round_authority)?;
    let last = rounds.last();
    let wall_ms = wall_span_ms(
        &store
            .attempt_wall(run_id)
            .map_err(|error| error.to_string())?,
    );
    // A Ledger that fails to rebuild must not hide the Campaign from the listing: the
    // summary is simply absent.
    let findings = LedgerProjection::rebuild(store, &cas, run_id)
        .ok()
        .map(|projection| findings_summary(&projection.into_ledger().finding_views()));
    Ok(CampaignView {
        id,
        label,
        authority_snapshot_id: opened.authority_snapshot_id,
        campaign_manifest_id: opened.campaign_manifest_id,
        subject_kind: manifest.subject_kind,
        base_snapshot_id: manifest.base_snapshot_id,
        last_subject_id,
        last_closed_round: last.and_then(|round| round.round),
        last_closed_epoch: last.and_then(|round| round.epoch),
        verdict: last.map(|round| round.verdict.clone()),
        rounds,
        wall_ms,
        findings,
    })
}

fn print_campaigns_text(view: &CampaignListView) {
    println!("Campaigns: {}", view.campaigns.len());
    for campaign in &view.campaigns {
        println!("{} ({})", campaign.label.escape_debug(), campaign.id);
        println!(
            "  authority: snapshot {}; manifest {}",
            campaign.authority_snapshot_id, campaign.campaign_manifest_id
        );
        println!(
            "  subject: {}{}",
            campaign.subject_kind,
            campaign
                .base_snapshot_id
                .as_deref()
                .map(|base| format!("; base {base}"))
                .unwrap_or_default()
        );
        println!(
            "  last subject: {}",
            campaign.last_subject_id.as_deref().unwrap_or("not started")
        );
        match last_closed_summary(
            campaign.last_closed_round,
            campaign.last_closed_epoch,
            campaign.verdict.as_deref(),
        ) {
            Some(summary) => println!("  last closed: {summary}"),
            None => println!("  last closed: none"),
        }
        if let Some(wall) = campaign.wall_ms {
            println!("  wall: {}", human_duration(wall));
        }
        if let Some(findings) = &campaign.findings {
            println!("  findings: {}", findings_summary_line(findings));
        }
        println!("  history:");
        if campaign.rounds.is_empty() {
            println!("    none");
        }
        for round in &campaign.rounds {
            println!(
                "    run {}: round {} epoch {}; {}; reported tokens {}",
                round.run,
                optional_number(round.round),
                optional_number(round.epoch),
                round.verdict,
                optional_tokens(round.reported_tokens)
            );
        }
    }
    println!("Problems: {}", view.problems.len());
    for problem in &view.problems {
        println!(
            "  {}: {}",
            problem.directory.escape_debug(),
            problem.reason.escape_debug()
        );
    }
}

#[derive(serde::Serialize)]
struct ReviewReportView {
    schema: &'static str,
    campaign: String,
    runs_recorded: usize,
    ledger_round: u32,
    final_verdict: Option<String>,
    rounds: Vec<ReportRoundView>,
    spend: Vec<RoundSpendView>,
    demands: Vec<review_core::DemandSetEntryV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded_not_gathered: Option<LatestRoundEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wall_ms: Option<u64>,
    findings_summary: FindingsSummaryView,
    findings: Vec<review_store::Finding>,
}

#[derive(serde::Serialize)]
struct ReportRoundView {
    run: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    round: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    epoch: Option<u32>,
    verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reported_tokens: Option<u64>,
}

#[derive(serde::Serialize)]
struct RoundSpendView {
    round: u32,
    epoch: u32,
    spent_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    wall_ms: Option<u64>,
    reviewers: Vec<ReviewerSpendView>,
}

#[derive(serde::Serialize)]
struct ReviewerSpendView {
    reviewer: String,
    spent_tokens: u64,
    attempt_tokens: u64,
    provider_tokens: u64,
    attempts: Vec<AttemptSpendView>,
    provider_operations: Vec<ProviderSpendView>,
}

#[derive(serde::Serialize)]
struct AttemptSpendView {
    attempt_id: String,
    outcome: String,
    spent_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// The reservation that bounded this Attempt: the node's own cap, or the pipeline's.
    #[serde(skip_serializing_if = "Option::is_none")]
    reserved: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wall: Option<AttemptWallView>,
}

/// Wall-clock and provider usage from the store's sidecar; absent when the Attempt predates it.
#[derive(serde::Serialize, Clone)]
struct AttemptWallView {
    started_unix_ms: u64,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<review_store::AttemptUsage>,
}

/// Findings by disposition, so precision is a number rather than a feeling.
#[derive(serde::Serialize, Clone, Copy, Default)]
struct FindingsSummaryView {
    open: usize,
    pending_verification: usize,
    fixed: usize,
    rejected: usize,
    wontfix: usize,
    contested: usize,
}

fn findings_summary(findings: &[review_store::Finding]) -> FindingsSummaryView {
    findings
        .iter()
        .fold(FindingsSummaryView::default(), |mut summary, finding| {
            match finding.status {
                Status::Open => summary.open += 1,
                Status::PendingVerification => summary.pending_verification += 1,
                Status::Fixed => summary.fixed += 1,
                Status::Rejected => summary.rejected += 1,
                Status::Wontfix => summary.wontfix += 1,
                Status::Contested => summary.contested += 1,
            }
            summary
        })
}

fn findings_summary_line(summary: &FindingsSummaryView) -> String {
    format!(
        "{} open, {} pending, {} fixed, {} rejected, {} wontfix, {} contested",
        summary.open,
        summary.pending_verification,
        summary.fixed,
        summary.rejected,
        summary.wontfix,
        summary.contested
    )
}

/// Wall-clock a set of Rounds took: per (round, epoch), first Attempt start to last Attempt end,
/// summed across Rounds. `None` when nothing was recorded.
fn wall_span_ms(rows: &[review_store::AttemptWall]) -> Option<u64> {
    let mut spans: BTreeMap<(u32, u32), (u64, u64)> = BTreeMap::new();
    for row in rows {
        let end = row.started_unix_ms.saturating_add(row.elapsed_ms);
        spans
            .entry((row.round, row.epoch))
            .and_modify(|(start, finish)| {
                *start = (*start).min(row.started_unix_ms);
                *finish = (*finish).max(end);
            })
            .or_insert((row.started_unix_ms, end));
    }
    if spans.is_empty() {
        return None;
    }
    Some(spans.values().fold(0_u64, |sum, (start, finish)| {
        sum.saturating_add(finish.saturating_sub(*start))
    }))
}

/// Attaches sidecar rows to the spend view and returns the campaign's wall-clock total.
fn attach_attempt_wall(
    spend: &mut [RoundSpendView],
    rows: &[review_store::AttemptWall],
) -> Option<u64> {
    let by_attempt: BTreeMap<&str, &review_store::AttemptWall> = rows
        .iter()
        .map(|row| (row.attempt_id.as_str(), row))
        .collect();
    for round in spend.iter_mut() {
        let mut in_round = Vec::new();
        for reviewer in &mut round.reviewers {
            for attempt in &mut reviewer.attempts {
                if let Some(row) = by_attempt.get(attempt.attempt_id.as_str()) {
                    attempt.wall = Some(AttemptWallView {
                        started_unix_ms: row.started_unix_ms,
                        elapsed_ms: row.elapsed_ms,
                        usage: row.usage.clone(),
                    });
                    in_round.push((*row).clone());
                }
            }
        }
        round.wall_ms = wall_span_ms(&in_round);
    }
    wall_span_ms(rows)
}

fn human_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if ms < 1000 {
        format!("{ms}ms")
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn usage_summary(usage: &review_store::AttemptUsage) -> String {
    let mut parts = Vec::new();
    for (label, value) in [
        ("in", usage.input_tokens),
        ("out", usage.output_tokens),
        ("cache-read", usage.cache_read_tokens),
        ("cache-write", usage.cache_write_tokens),
        ("reasoning", usage.reasoning_tokens),
    ] {
        if let Some(value) = value {
            parts.push(format!("{label} {value}"));
        }
    }
    if parts.is_empty() {
        format!("chargeable {}", usage.chargeable_tokens)
    } else {
        parts.join(", ")
    }
}

fn attempt_wall_suffix(wall: Option<&AttemptWallView>) -> String {
    wall.map(|wall| {
        format!(
            ", {}{}",
            human_duration(wall.elapsed_ms),
            wall.usage
                .as_ref()
                .map(|usage| format!(" ({})", usage_summary(usage)))
                .unwrap_or_default()
        )
    })
    .unwrap_or_default()
}

#[derive(serde::Serialize)]
struct ProviderSpendView {
    operation_id: String,
    provider_id: String,
    capability_id: String,
    state: String,
    spent_tokens: u64,
}

struct RoundSpendAccumulator {
    round: u32,
    epoch: u32,
    reviewers: BTreeMap<String, ReviewerSpendAccumulator>,
}

#[derive(Default)]
struct ReviewerSpendAccumulator {
    attempts: BTreeMap<String, AttemptSpendAccumulator>,
    providers: BTreeMap<String, ProviderSpendAccumulator>,
}

struct AttemptSpendAccumulator {
    outcome: String,
    spent_tokens: u64,
    /// The reservation that bounded this Attempt — its node cap, or the pipeline's.
    reserved: Option<u64>,
    broker_observed_tokens: u64,
    detail: Option<String>,
    terminal: bool,
}

struct ProviderSpendAccumulator {
    provider_id: String,
    capability_id: String,
    state: review_core::ProviderOperationStateV1,
    charged_tokens: u64,
    reserved_tokens: u64,
    failure: bool,
}

fn print_report(options: &ReportOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let ledger = LedgerProjection::rebuild(&store, &cas, &run_id)
        .map_err(|e| e.to_string())?
        .into_ledger();
    print_scope_authority_warnings(&ledger);
    let events = store.replay(&run_id).map_err(|e| e.to_string())?;
    let reports: Vec<_> = events
        .iter()
        .filter(|event| event.event_type.is_run_report())
        .collect();
    let round_authority = report_round_authority(&events)?;
    let rounds = report_rounds(&reports, &round_authority)?;
    let recorded_not_gathered = latest_round_evidence(&events, &cas)?.filter(|evidence| {
        evidence.ledger_was_not_produced() && !evidence.available_node_results.is_empty()
    });
    let mut spend = report_spend(&events, &round_authority)?;
    let wall_rows = store.attempt_wall(&run_id).map_err(|e| e.to_string())?;
    let wall_ms = attach_attempt_wall(&mut spend, &wall_rows);
    let findings = ledger.finding_views();
    let view = ReviewReportView {
        schema: "af/review-report@1",
        campaign: options.campaign.clone(),
        runs_recorded: reports.len(),
        ledger_round: ledger.round,
        final_verdict: reports
            .last()
            .map(|event| report_verdict(event))
            .transpose()?,
        rounds,
        spend,
        demands: ledger.demand_views(),
        recorded_not_gathered,
        wall_ms,
        findings_summary: findings_summary(&findings),
        findings,
    };

    match options.format {
        ReportFormat::Markdown => print_report_markdown(&view),
        ReportFormat::Text => print_report_text(&view),
        ReportFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&view).map_err(|error| error.to_string())?
        ),
    }
    Ok(())
}

fn report_rounds(
    reports: &[&review_core::RunEvent],
    round_authority: &BTreeMap<String, (u32, u32)>,
) -> Result<Vec<ReportRoundView>, String> {
    reports
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let authority = event
                .causation_id
                .as_deref()
                .and_then(|causation| round_authority.get(causation));
            Ok(ReportRoundView {
                run: index + 1,
                round: authority.map(|(round, _)| *round),
                epoch: authority.map(|(_, epoch)| *epoch),
                verdict: report_verdict(event)?,
                reported_tokens: event.payload.get("spent_tokens").and_then(|v| v.as_u64()),
            })
        })
        .collect()
}

fn report_round_authority(
    events: &[review_core::RunEvent],
) -> Result<BTreeMap<String, (u32, u32)>, String> {
    events
        .iter()
        .filter(|event| event.event_type == EventType::RoundStartedV1)
        .map(|event| {
            let payload: review_core::RoundStartedPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
            Ok((event.event_id.clone(), (payload.round, payload.epoch)))
        })
        .collect()
}

fn report_spend(
    events: &[review_core::RunEvent],
    round_authority: &BTreeMap<String, (u32, u32)>,
) -> Result<Vec<RoundSpendView>, String> {
    let mut rounds: BTreeMap<String, RoundSpendAccumulator> = round_authority
        .iter()
        .map(|(event_id, (round, epoch))| {
            (
                event_id.clone(),
                RoundSpendAccumulator {
                    round: *round,
                    epoch: *epoch,
                    reviewers: BTreeMap::new(),
                },
            )
        })
        .collect();

    for event in events {
        let Some(round_id) = event.causation_id.as_deref() else {
            continue;
        };
        let Some(round) = rounds.get_mut(round_id) else {
            continue;
        };
        match event.event_type {
            EventType::AttemptDispatchedV1 => {
                let payload: review_core::event::AttemptDispatchedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let (node, attempt) = event_attempt_identity(event)?;
                round
                    .reviewers
                    .entry(node.to_string())
                    .or_default()
                    .attempts
                    .insert(
                        attempt.to_string(),
                        AttemptSpendAccumulator {
                            outcome: "running".to_string(),
                            spent_tokens: payload.reserved.unwrap_or(0),
                            reserved: payload.reserved,
                            broker_observed_tokens: 0,
                            detail: None,
                            terminal: false,
                        },
                    );
            }
            EventType::ReviewerExecutionBoundV1 => {
                let binding: review_core::ReviewerExecutionBindingV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let authority = review_core::broker_authority_usage(&binding.operations)?;
                let (node, attempt_id) = event_attempt_identity(event)?;
                let attempt = round
                    .reviewers
                    .get_mut(node)
                    .and_then(|reviewer| reviewer.attempts.get_mut(attempt_id))
                    .ok_or("Reviewer Execution Binding has no dispatched Attempt")?;
                if !attempt.terminal {
                    attempt.spent_tokens = attempt.spent_tokens.max(authority);
                }
            }
            EventType::BrokerOperationCompletedV1 => {
                let receipt: review_core::BrokerOperationReceiptV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let (node, attempt_id) = event_attempt_identity(event)?;
                let attempt = round
                    .reviewers
                    .get_mut(node)
                    .and_then(|reviewer| reviewer.attempts.get_mut(attempt_id))
                    .ok_or("Broker operation receipt has no dispatched Attempt")?;
                attempt.broker_observed_tokens = attempt
                    .broker_observed_tokens
                    .checked_add(receipt.charged_usage)
                    .ok_or("reported Broker spend overflow")?;
                attempt.spent_tokens = attempt.spent_tokens.max(attempt.broker_observed_tokens);
            }
            EventType::AttemptAdmittedV1 => {
                let payload: review_core::event::AttemptAdmittedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                settle_attempt(round, event, payload.selection, payload.cost_tokens, None)?;
            }
            EventType::AttemptFailedV1 => {
                let payload: review_core::event::AttemptFailedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                settle_attempt(
                    round,
                    event,
                    "failed".to_string(),
                    payload.charged.unwrap_or(0),
                    Some(payload.error),
                )?;
            }
            EventType::AttemptFencedV1 => {
                let payload: review_core::event::AttemptFencedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                settle_attempt(
                    round,
                    event,
                    "fenced".to_string(),
                    payload.charged.unwrap_or(0),
                    Some(payload.reason),
                )?;
            }
            EventType::AttemptReleasedV1 => {
                let payload: review_core::event::AttemptReleasedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                settle_attempt(round, event, "released".to_string(), 0, Some(payload.error))?;
            }
            EventType::ProviderOperationTransitionV1 => {
                let payload: review_core::ProviderOperationTransitionPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let reviewer = round.reviewers.entry(payload.node_id.clone()).or_default();
                let operation = reviewer
                    .providers
                    .entry(payload.operation_id.clone())
                    .or_insert_with(|| ProviderSpendAccumulator {
                        provider_id: payload.provider_id.clone(),
                        capability_id: payload.capability_id.clone(),
                        state: payload.state,
                        charged_tokens: 0,
                        reserved_tokens: 0,
                        failure: false,
                    });
                operation.charged_tokens = operation
                    .charged_tokens
                    .checked_add(payload.charged_tokens)
                    .ok_or("reported Provider spend overflow")?;
                operation.state = payload.state;
                operation.reserved_tokens = payload.reserved_tokens;
                operation.failure = payload.failure_class.is_some();
            }
            _ => {}
        }
    }

    let mut views = rounds
        .into_values()
        .map(round_spend_view)
        .collect::<Result<Vec<_>, String>>()?;
    views.sort_by_key(|view| (view.round, view.epoch));
    Ok(views)
}

fn event_attempt_identity(event: &review_core::RunEvent) -> Result<(&str, &str), String> {
    Ok((
        event
            .node_id
            .as_deref()
            .ok_or_else(|| format!("{} has no reviewer node", event.event_type))?,
        event
            .attempt_id
            .as_deref()
            .ok_or_else(|| format!("{} has no Attempt ID", event.event_type))?,
    ))
}

fn settle_attempt(
    round: &mut RoundSpendAccumulator,
    event: &review_core::RunEvent,
    outcome: String,
    spent_tokens: u64,
    detail: Option<String>,
) -> Result<(), String> {
    let (node, attempt_id) = event_attempt_identity(event)?;
    let attempt = round
        .reviewers
        .entry(node.to_string())
        .or_default()
        .attempts
        .entry(attempt_id.to_string())
        .or_insert_with(|| AttemptSpendAccumulator {
            outcome: "running".to_string(),
            spent_tokens: 0,
            reserved: None,
            broker_observed_tokens: 0,
            detail: None,
            terminal: false,
        });
    if attempt.terminal {
        // A late response to an already-fenced Attempt is durably quarantined but must not be
        // charged twice. The first terminal lifecycle event owns its operator-visible outcome.
        return Ok(());
    }
    attempt.outcome = outcome;
    attempt.spent_tokens = spent_tokens.max(attempt.broker_observed_tokens);
    attempt.detail = detail;
    attempt.terminal = true;
    Ok(())
}

fn round_spend_view(round: RoundSpendAccumulator) -> Result<RoundSpendView, String> {
    let mut spent_tokens = 0_u64;
    let mut reviewers = Vec::new();
    for (reviewer, accumulator) in round.reviewers {
        let attempts = accumulator
            .attempts
            .into_iter()
            .map(|(attempt_id, attempt)| AttemptSpendView {
                attempt_id,
                outcome: attempt.outcome,
                spent_tokens: attempt.spent_tokens,
                detail: attempt.detail,
                reserved: attempt.reserved,
                wall: None,
            })
            .collect::<Vec<_>>();
        let attempt_tokens = attempts.iter().try_fold(0_u64, |sum, attempt| {
            sum.checked_add(attempt.spent_tokens)
                .ok_or("reported Attempt spend overflow")
        })?;
        let provider_operations = accumulator
            .providers
            .into_iter()
            .map(|(operation_id, provider)| {
                let outstanding = if provider.state
                    == review_core::ProviderOperationStateV1::Running
                    && !provider.failure
                {
                    provider.reserved_tokens
                } else {
                    0
                };
                Ok(ProviderSpendView {
                    operation_id,
                    provider_id: provider.provider_id,
                    capability_id: provider.capability_id,
                    state: provider_state_label(provider.state).to_string(),
                    spent_tokens: provider
                        .charged_tokens
                        .checked_add(outstanding)
                        .ok_or("reported Provider spend overflow")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let provider_tokens = provider_operations
            .iter()
            .try_fold(0_u64, |sum, operation| {
                sum.checked_add(operation.spent_tokens)
                    .ok_or("reported Provider spend overflow")
            })?;
        let reviewer_tokens = attempt_tokens
            .checked_add(provider_tokens)
            .ok_or("reported reviewer spend overflow")?;
        spent_tokens = spent_tokens
            .checked_add(reviewer_tokens)
            .ok_or("reported Round spend overflow")?;
        reviewers.push(ReviewerSpendView {
            reviewer,
            spent_tokens: reviewer_tokens,
            attempt_tokens,
            provider_tokens,
            attempts,
            provider_operations,
        });
    }
    Ok(RoundSpendView {
        round: round.round,
        epoch: round.epoch,
        spent_tokens,
        wall_ms: None,
        reviewers,
    })
}

fn print_report_text(report: &ReviewReportView) {
    println!("Review campaign: {}", report.campaign);
    println!("Runs recorded: {}", report.runs_recorded);
    println!("Ledger round: {}", report.ledger_round);
    println!(
        "Final verdict: {}",
        report.final_verdict.as_deref().unwrap_or("not recorded")
    );
    if let Some(wall) = report.wall_ms {
        println!("Wall-clock: {}", human_duration(wall));
    }
    println!(
        "Findings: {}",
        findings_summary_line(&report.findings_summary)
    );
    println!("Rounds:");
    if report.rounds.is_empty() {
        println!("  none");
    }
    for round in &report.rounds {
        println!(
            "  run {} (round {} epoch {}): {}; reported tokens {}",
            round.run,
            optional_number(round.round),
            optional_number(round.epoch),
            round.verdict,
            optional_tokens(round.reported_tokens)
        );
    }
    println!("Spend:");
    for round in &report.spend {
        println!(
            "  round {} epoch {}: {} tokens{}",
            round.round,
            round.epoch,
            round.spent_tokens,
            round
                .wall_ms
                .map(|ms| format!(", {}", human_duration(ms)))
                .unwrap_or_default()
        );
        for reviewer in &round.reviewers {
            println!(
                "    {}: {} tokens (attempts {}, providers {})",
                reviewer.reviewer,
                reviewer.spent_tokens,
                reviewer.attempt_tokens,
                reviewer.provider_tokens
            );
            for attempt in &reviewer.attempts {
                println!(
                    "      attempt {}: {}, {} tokens{}{}{}",
                    attempt.attempt_id,
                    attempt.outcome,
                    attempt.spent_tokens,
                    attempt
                        .reserved
                        .map(|cap| format!(" (cap {cap})"))
                        .unwrap_or_default(),
                    attempt_wall_suffix(attempt.wall.as_ref()),
                    attempt
                        .detail
                        .as_deref()
                        .map(|detail| format!(" - {}", one_line(detail)))
                        .unwrap_or_default()
                );
            }
        }
    }
    println!("Demands:");
    if report.demands.is_empty() {
        println!("  none");
    }
    for demand in &report.demands {
        println!(
            "  [{}; {}] {} ({})",
            demand_requirement_label(demand.requirement),
            demand_status_label(demand.status),
            one_line(&demand.claim),
            demand.demand_id
        );
        println!("    why: {}", one_line(&demand.why));
        println!(
            "    suggested method: {}",
            one_line(&demand.suggested_method)
        );
        println!("    source: {}", demand.source);
    }
    print_recorded_not_gathered_text(report.recorded_not_gathered.as_ref());
    println!("Findings:");
    if report.findings.is_empty() {
        println!("  none");
    }
    for finding in &report.findings {
        println!(
            "  [{}; scope={}; severity={}; effective={}] {} ({}) at {}:{}",
            finding.status.as_str(),
            finding.convergence_scope_label(),
            severity_label(finding.severity),
            finding
                .convergence_severity
                .map(severity_label)
                .unwrap_or("-"),
            one_line(&finding.title),
            finding.key,
            finding.file,
            finding
                .line
                .map_or("-".to_string(), |line| line.to_string())
        );
        println!("    body: {}", one_line(&finding.body));
        println!(
            "    fix: {}",
            one_line(
                finding
                    .fix
                    .as_deref()
                    .unwrap_or("unavailable: artifact-less legacy import")
            )
        );
        for transition in &finding.history {
            println!(
                "    history round {}: {} - {}",
                transition.round,
                transition_label(transition.kind),
                one_line(transition.note.as_deref().unwrap_or("(no note)"))
            );
        }
    }
}

fn print_report_markdown(report: &ReviewReportView) {
    println!("# Review campaign `{}`", report.campaign);
    println!();
    println!("- Runs recorded: {}", report.runs_recorded);
    println!("- Ledger round: {}", report.ledger_round);
    println!(
        "- Final verdict: {}",
        report.final_verdict.as_deref().unwrap_or("not recorded")
    );
    if let Some(wall) = report.wall_ms {
        println!("- Wall-clock: {}", human_duration(wall));
    }
    println!(
        "- Findings: {}",
        findings_summary_line(&report.findings_summary)
    );
    println!();
    println!("## Runs");
    println!();
    println!("| Run | Round | Epoch | Verdict | Tokens |");
    println!("| ---: | ---: | ---: | --- | ---: |");
    for round in &report.rounds {
        println!(
            "| {} | {} | {} | {} | {} |",
            round.run,
            optional_number(round.round),
            optional_number(round.epoch),
            round.verdict,
            optional_tokens(round.reported_tokens)
        );
    }
    println!();
    println!("## Spend");
    println!();
    println!(
        "| Round | Epoch | Reviewer | Attempt tokens | Provider tokens | Total tokens | Round wall |"
    );
    println!("| ---: | ---: | --- | ---: | ---: | ---: | ---: |");
    for round in &report.spend {
        let wall = round
            .wall_ms
            .map(human_duration)
            .unwrap_or_else(|| "-".to_string());
        if round.reviewers.is_empty() {
            println!(
                "| {} | {} | - | 0 | 0 | 0 | {wall} |",
                round.round, round.epoch
            );
        }
        for reviewer in &round.reviewers {
            println!(
                "| {} | {} | {} | {} | {} | {} | {wall} |",
                round.round,
                round.epoch,
                reviewer.reviewer,
                reviewer.attempt_tokens,
                reviewer.provider_tokens,
                reviewer.spent_tokens
            );
        }
    }
    println!();
    println!("### Attempts");
    for round in &report.spend {
        for reviewer in &round.reviewers {
            for attempt in &reviewer.attempts {
                println!();
                println!(
                    "- Round {}, **{}**, Attempt `{}`: {}, {} tokens{}{}{}",
                    round.round,
                    reviewer.reviewer,
                    attempt.attempt_id,
                    attempt.outcome,
                    attempt.spent_tokens,
                    attempt
                        .reserved
                        .map(|cap| format!(" (cap {cap})"))
                        .unwrap_or_default(),
                    attempt_wall_suffix(attempt.wall.as_ref()),
                    attempt
                        .detail
                        .as_deref()
                        .map(|detail| format!(" — {}", markdown_line(detail)))
                        .unwrap_or_default()
                );
            }
        }
    }
    println!();
    println!("## Demands");
    if report.demands.is_empty() {
        println!();
        println!("None.");
    }
    for demand in &report.demands {
        println!();
        println!(
            "- **[{}, {}] {}** (`{}`)",
            demand_requirement_label(demand.requirement),
            demand_status_label(demand.status),
            demand.claim,
            demand.demand_id
        );
        println!("  - Why: {}", markdown_line(&demand.why));
        println!(
            "  - Suggested method: {}",
            markdown_line(&demand.suggested_method)
        );
        println!("  - Source: {}", demand.source);
    }
    print_recorded_not_gathered_markdown(report.recorded_not_gathered.as_ref());
    println!();
    println!("## Findings");
    for effective_severity in [
        Some(Severity::Blocker),
        Some(Severity::Major),
        Some(Severity::Minor),
        None,
    ] {
        println!();
        let heading =
            effective_severity.map_or("Recorded, not blocking this Subject", |severity| {
                match severity {
                    Severity::Blocker => "Blocker",
                    Severity::Major => "Major",
                    Severity::Minor => "Minor",
                }
            });
        println!("### {heading}");
        let matching = report
            .findings
            .iter()
            .filter(|finding| finding.convergence_severity == effective_severity)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            println!();
            println!("None.");
            continue;
        }
        for finding in matching {
            println!();
            println!(
                "- **[{}, scope={}, severity={}, effective={}] {}** (`{}`) at `{}:{}`",
                finding.status.as_str(),
                finding.convergence_scope_label(),
                severity_label(finding.severity),
                finding
                    .convergence_severity
                    .map(severity_label)
                    .unwrap_or("-"),
                finding.title,
                finding.key,
                finding.file,
                finding
                    .line
                    .map_or("-".to_string(), |line| line.to_string())
            );
            println!("  - Body: {}", markdown_line(&finding.body));
            println!(
                "  - Fix: {}",
                markdown_line(
                    finding
                        .fix
                        .as_deref()
                        .unwrap_or("unavailable: artifact-less legacy import")
                )
            );
            let evidence = finding
                .reports
                .iter()
                .map(|attached| {
                    if attached.report_id.is_empty() {
                        format!(
                            "{} round {} scope={} at {}:{} (legacy import)",
                            attached.source,
                            attached.round,
                            attached.scope_label(),
                            attached.file,
                            attached
                                .line
                                .map_or("-".to_string(), |line| line.to_string())
                        )
                    } else {
                        format!(
                            "{} round {} scope={} at {}:{} `{}`",
                            attached.source,
                            attached.round,
                            attached.scope_label(),
                            attached.file,
                            attached
                                .line
                                .map_or("-".to_string(), |line| line.to_string()),
                            attached.report_id
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            println!("  - Reports: {evidence}");
            for transition in &finding.history {
                println!(
                    "  - Resolution/history, round {}: {} - {}",
                    transition.round,
                    transition_label(transition.kind),
                    markdown_line(transition.note.as_deref().unwrap_or("(no note)"))
                );
            }
        }
    }
}

fn print_recorded_not_gathered_text(evidence: Option<&LatestRoundEvidence>) {
    let Some(evidence) = evidence else { return };
    println!("Recorded, not gathered:");
    println!("  reason: {}", evidence.absence_reason());
    for result in &evidence.available_node_results {
        println!(
            "  {} attempt {}: result {}, {} tokens, severities {}",
            result.node,
            result.attempt_id,
            result.result_artifact_id,
            result.spend_tokens,
            if result.severities.is_empty() {
                "none recorded".to_string()
            } else {
                result.severities.join(", ")
            }
        );
    }
}

fn print_recorded_not_gathered_markdown(evidence: Option<&LatestRoundEvidence>) {
    let Some(evidence) = evidence else { return };
    println!();
    println!("## Recorded, not gathered");
    println!();
    println!(
        "The latest Round did not produce a Ledger because {}. These admitted results remain evidence only; they are not Findings, a clean Ledger, or convergence input.",
        evidence.absence_reason()
    );
    for result in &evidence.available_node_results {
        println!();
        println!(
            "- **{}**, Attempt `{}`, result `{}`, spend {} tokens, severities: {}",
            result.node,
            result.attempt_id,
            result.result_artifact_id,
            result.spend_tokens,
            if result.severities.is_empty() {
                "none recorded".to_string()
            } else {
                result.severities.join(", ")
            }
        );
        for finding in &result.findings {
            println!(
                "  - [{}] {} — {}",
                finding["severity"].as_str().unwrap_or("unknown"),
                markdown_line(finding["title"].as_str().unwrap_or("untitled finding")),
                markdown_line(finding["body"].as_str().unwrap_or("(no body)"))
            );
        }
    }
}

fn provider_state_label(state: review_core::ProviderOperationStateV1) -> &'static str {
    match state {
        review_core::ProviderOperationStateV1::Running => "running",
        review_core::ProviderOperationStateV1::WaitingForHuman => "waiting_for_human",
        review_core::ProviderOperationStateV1::Resumed => "resumed",
        review_core::ProviderOperationStateV1::Done => "done",
        review_core::ProviderOperationStateV1::Failed => "failed",
    }
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Minor => "minor",
        Severity::Major => "major",
        Severity::Blocker => "blocker",
    }
}

fn demand_requirement_label(requirement: review_core::DemandRequirement) -> &'static str {
    match requirement {
        review_core::DemandRequirement::Required => "required",
        review_core::DemandRequirement::Advisory => "advisory",
    }
}

fn demand_status_label(status: review_core::DemandStatus) -> &'static str {
    match status {
        review_core::DemandStatus::Open => "open",
        review_core::DemandStatus::Satisfied => "satisfied",
        review_core::DemandStatus::Stale => "stale",
        review_core::DemandStatus::Waived => "waived",
    }
}

fn transition_label(kind: review_store::ledger::TransitionKind) -> String {
    match kind {
        review_store::ledger::TransitionKind::Reported => "reported".to_string(),
        review_store::ledger::TransitionKind::Duplicate => "duplicate".to_string(),
        review_store::ledger::TransitionKind::Escalated => "escalated".to_string(),
        review_store::ledger::TransitionKind::Reopened => "reopened".to_string(),
        review_store::ledger::TransitionKind::AdoptedWhileDeclined => {
            "adopted_while_declined".to_string()
        }
        review_store::ledger::TransitionKind::AuthorityRecovered => {
            "authority_recovered".to_string()
        }
        review_store::ledger::TransitionKind::Attested => "attested".to_string(),
        review_store::ledger::TransitionKind::Challenged => "challenged".to_string(),
        review_store::ledger::TransitionKind::Resolved(status) => {
            format!("resolved:{}", status.as_str())
        }
    }
}

fn optional_tokens(tokens: Option<u64>) -> String {
    tokens.map_or_else(|| "-".to_string(), |tokens| tokens.to_string())
}

fn optional_number(number: Option<u32>) -> String {
    number.map_or_else(|| "-".to_string(), |number| number.to_string())
}

fn last_closed_summary(
    round: Option<u32>,
    epoch: Option<u32>,
    verdict: Option<&str>,
) -> Option<String> {
    verdict.map(|verdict| {
        format!(
            "round {} epoch {}; {verdict}",
            optional_number(round),
            optional_number(epoch)
        )
    })
}

fn one_line(value: &str) -> String {
    value.lines().collect::<Vec<_>>().join(" ")
}

fn report_verdict(event: &review_core::RunEvent) -> Result<String, String> {
    match event.event_type {
        EventType::RunReportV1 => event.payload["verdict"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "RunReport@1 has no string verdict".to_string()),
        EventType::RunReportV2 => {
            let report: RunReportPayloadV2 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(match report.verdict {
                RunVerdictV2::Pass => "pass".to_string(),
                RunVerdictV2::Fail {
                    reason: RunFailureReasonV2::NotConverged,
                } => "fail (not_converged)".to_string(),
                RunVerdictV2::Fail {
                    reason: RunFailureReasonV2::Exhausted,
                } => "fail (exhausted)".to_string(),
                RunVerdictV2::Incomplete { missing_nodes } => {
                    format!("incomplete ({} missing nodes)", missing_nodes.len())
                }
            })
        }
        EventType::RunReportV3 => {
            let report: RunReportPayloadV3 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(render_verdict_v3(report.verdict))
        }
        EventType::RunReportV4 => {
            let report: RunReportPayloadV4 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(render_verdict_v3(report.verdict))
        }
        EventType::RunReportV5 => {
            let report: RunReportPayloadV5 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(render_verdict_v3(report.verdict))
        }
        _ => Err(format!("{} is not a run report", event.event_type)),
    }
}

fn render_verdict_v3(verdict: RunVerdictV3) -> String {
    match verdict {
        RunVerdictV3::Pass => "pass".to_string(),
        RunVerdictV3::Fail {
            reason: RunFailureReasonV3::NotConverged,
        } => "fail (not_converged)".to_string(),
        RunVerdictV3::Fail {
            reason: RunFailureReasonV3::AuthorityUnavailable,
        } => "fail (authority_unavailable)".to_string(),
        RunVerdictV3::Fail {
            reason: RunFailureReasonV3::Exhausted,
        } => "fail (exhausted)".to_string(),
        RunVerdictV3::Incomplete { missing_nodes } => {
            format!("incomplete ({} missing nodes)", missing_nodes.len())
        }
    }
}

fn markdown_line(value: &str) -> String {
    value.lines().collect::<Vec<_>>().join(" ")
}

fn print_scope_authority_warnings(ledger: &Ledger) {
    for failure in ledger.scope_authority_failures() {
        match failure.authority {
            review_store::ScopeAuthorityKind::RoundBinding => eprintln!(
                "warning: round {} Report Scope is unknown: round binding disagrees for Subject {}: {}",
                failure.round, failure.authority_id, failure.reason
            ),
            authority => eprintln!(
                "warning: round {} Report Scope is unknown: {:?} authority {} is unavailable: {}",
                failure.round, authority, failure.authority_id, failure.reason
            ),
        }
    }
}

fn resolve(options: &ResolveOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let mut ingest = Ingest::new(&mut store, &cas, run_id).map_err(|e| e.to_string())?;
    print_scope_authority_warnings(ingest.ledger());
    if ingest.ledger().get(&options.key).is_none() {
        return Err(format!("no finding with key {}", options.key));
    }
    let outcome = match options.status.as_str() {
        "rejected" => review_core::FindingResolutionOutcome::Rejected,
        "wontfix-tracked" => review_core::FindingResolutionOutcome::WontfixTracked,
        "fixed" => {
            return Err(
                "fixed requires `af review attest-change` followed by `af review verify-fix`"
                    .into(),
            );
        }
        other => return Err(format!("unsupported direct Resolution outcome `{other}`")),
    };
    let resolution_id = ingest
        .resolve_nonfixed(
            &options.key,
            outcome,
            &operator_actor(options.actor.as_deref())?,
            &options.policy_revision,
            &options.reason,
            options.evidence_ids.clone(),
            options.max_accepted_severity,
            options.tracking_reference.clone(),
            options.expires_at_policy_time,
        )
        .map_err(|e| e.to_string())?;
    let now = ingest
        .ledger()
        .finding_view(&options.key)
        .map(|f| f.status.as_str())
        .unwrap_or("?");
    println!("resolved {} -> {} ({resolution_id})", options.key, now);
    Ok(())
}

fn attest_change(options: &AttestChangeOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut ingest = Ingest::new(&mut store, &cas, campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?;
    let id = ingest
        .attest_change(
            &options.finding_id,
            options.regions.clone(),
            &operator_actor(options.actor.as_deref())?,
            &options.reason,
            options.evidence_ids.clone(),
        )
        .map_err(|e| e.to_string())?;
    println!(
        "attested {} -> pending-verification ({id})",
        options.finding_id
    );
    Ok(())
}

fn verify_fix(options: &VerifyFixOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut ingest = Ingest::new(&mut store, &cas, campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?;
    let (verification, resolution) = ingest
        .verify_fix(
            &options.finding_id,
            &options.attestation_id,
            &operator_actor(options.verifier.as_deref())?,
            &options.policy_revision,
            options.positive,
            &options.reason,
            options.evidence_ids.clone(),
        )
        .map_err(|e| e.to_string())?;
    match resolution {
        Some(resolution) => println!(
            "verified {} -> fixed (verification {verification}, resolution {resolution})",
            options.finding_id
        ),
        None => println!(
            "verification for {} was negative ({verification}); Finding remains pending-verification",
            options.finding_id
        ),
    }
    Ok(())
}

fn challenge_resolution(options: &ChallengeResolutionOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut ingest = Ingest::new(&mut store, &cas, campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?;
    let id = ingest
        .challenge_resolution(
            &options.finding_id,
            options.kind,
            &operator_actor(options.actor.as_deref())?,
            &options.reason,
            options.evidence_ids.clone(),
        )
        .map_err(|e| e.to_string())?;
    println!("challenged {} -> contested ({id})", options.finding_id);
    Ok(())
}

fn advance_policy_time(options: &PolicyTimeOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut ingest = Ingest::new(&mut store, &cas, campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?;
    let (time_id, challenges) = ingest
        .advance_policy_time(
            options.tick,
            &operator_actor(options.actor.as_deref())?,
            &options.reason,
        )
        .map_err(|e| e.to_string())?;
    println!(
        "policy time advanced to {} ({time_id}); {} tracked Resolution(s) challenged",
        options.tick,
        challenges.len()
    );
    Ok(())
}

fn group(options: &GroupOptions, undo: bool) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let mut ingest = Ingest::new(&mut store, &cas, run_id).map_err(|e| e.to_string())?;
    if undo {
        ingest
            .ungroup(&options.from, &options.into)
            .map_err(|e| e.to_string())?;
        println!("ungrouped {} from {}", options.from, options.into);
    } else {
        ingest
            .group(&options.from, &options.into)
            .map_err(|e| e.to_string())?;
        println!("grouped {} into {}", options.from, options.into);
    }
    Ok(())
}

fn operator_actor(configured: Option<&str>) -> Result<String, String> {
    configured
        .map(str::to_string)
        .or_else(|| std::env::var("USER").ok())
        .filter(|actor| !actor.trim().is_empty())
        .ok_or_else(|| "an operator actor is required (--actor or USER)".to_string())
}

fn add_evidence(options: &EvidenceAddOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut file = std::fs::File::open(&options.file)
        .map_err(|error| format!("opening Evidence {}: {error}", options.file.display()))?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let (content_artifact_id, _) = cas
        .put_reader_with_buffer(&mut file, &mut buffer)
        .map_err(|error| error.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let mut ingest = Ingest::new(&mut store, &cas, run_id).map_err(|e| e.to_string())?;
    let evidence_id = ingest
        .add_evidence(
            &options.demand_id,
            &content_artifact_id,
            &operator_actor(options.actor.as_deref())?,
        )
        .map_err(|error| error.to_string())?;
    println!("evidence {evidence_id} recorded for {}", options.demand_id);
    Ok(())
}

fn satisfy_evidence(options: &EvidenceSatisfyOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let mut ingest = Ingest::new(&mut store, &cas, run_id).map_err(|e| e.to_string())?;
    let satisfaction_id = ingest
        .satisfy_demand(
            &options.demand_id,
            &options.evidence_id,
            &options.policy_revision,
            &options.reason,
        )
        .map_err(|error| error.to_string())?;
    let reuse_actor = options
        .admit_reuse
        .then(|| operator_actor(options.actor.as_deref()))
        .transpose()?;
    let reuse_id = reuse_actor
        .map(|actor| {
            ingest
                .admit_evidence_reuse(
                    &options.demand_id,
                    &satisfaction_id,
                    &actor,
                    &options.policy_revision,
                    &options.reason,
                )
                .map_err(|error| error.to_string())
        })
        .transpose()?;
    println!(
        "demand {} satisfied by {} under {} ({satisfaction_id})",
        options.demand_id, options.evidence_id, options.policy_revision
    );
    if let Some(reuse_id) = reuse_id {
        println!("future-Subject Evidence reuse admitted ({reuse_id})");
    }
    Ok(())
}

fn waive_demand(options: &DemandWaiveOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let mut ingest = Ingest::new(&mut store, &cas, run_id).map_err(|e| e.to_string())?;
    let waiver_id = ingest
        .waive_demand(
            &options.demand_id,
            &operator_actor(options.actor.as_deref())?,
            &options.policy_revision,
            &options.reason,
        )
        .map_err(|error| error.to_string())?;
    println!("demand {} waived ({waiver_id})", options.demand_id);
    Ok(())
}

struct CandidateIdentity {
    version: &'static str,
    executable: String,
    binary_sha256: String,
}

struct ReviewAuthority {
    authority_snapshot_id: String,
    campaign_manifest_id: String,
    subject_id: String,
    head_snapshot_id: String,
    round: u32,
    epoch: u32,
}

fn candidate_identity() -> Result<CandidateIdentity, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let bytes = std::fs::read(&executable).map_err(|error| error.to_string())?;
    Ok(CandidateIdentity {
        version: env!("CARGO_PKG_VERSION"),
        executable: executable.display().to_string(),
        binary_sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
    })
}

fn verdict_value(verdict: &RunVerdict) -> serde_json::Value {
    match verdict {
        RunVerdict::Pass => serde_json::json!({"kind": "clean"}),
        RunVerdict::Fail(Verdict::NotConverged) => {
            serde_json::json!({"kind": "fail", "reason": "not_converged"})
        }
        RunVerdict::Fail(Verdict::Exhausted) => {
            serde_json::json!({"kind": "fail", "reason": "exhausted"})
        }
        RunVerdict::Fail(Verdict::Converged) => {
            serde_json::json!({"kind": "fail", "reason": "invalid_converged_failure"})
        }
        RunVerdict::Incomplete { missing } => serde_json::json!({
            "kind": "incomplete",
            "missing_nodes": missing.iter().map(|(node, reason)| {
                serde_json::json!({"node": node, "reason": reason})
            }).collect::<Vec<_>>(),
        }),
    }
}

fn next_action_value(mode: CampaignMode, verdict: &RunVerdict) -> serde_json::Value {
    match (mode, verdict) {
        (_, RunVerdict::Pass) => serde_json::json!({
            "kind": "done",
            "start_another_campaign": false,
        }),
        (CampaignMode::Light, RunVerdict::Fail(_)) => serde_json::json!({
            "kind": "fix_then_gate",
            "start_another_campaign": false,
            "message": "Fix the concrete findings, run the deterministic project gate, then stop. Do not start another review Campaign; --heavy requires an explicit human choice.",
        }),
        (CampaignMode::Light, RunVerdict::Incomplete { .. }) => serde_json::json!({
            "kind": "resume_incomplete_round",
            "start_another_campaign": false,
            "message": "Resume this exact incomplete light Round. Do not start a replacement Campaign.",
        }),
        (CampaignMode::Heavy, RunVerdict::Fail(Verdict::NotConverged)) => serde_json::json!({
            "kind": "continue_campaign",
            "start_another_campaign": false,
        }),
        (CampaignMode::Heavy, RunVerdict::Fail(Verdict::Exhausted)) => serde_json::json!({
            "kind": "human_decision",
            "start_another_campaign": false,
            "message": "The heavy Campaign exhausted its pinned convergence window. Do not start another Campaign without an explicit human decision.",
        }),
        (CampaignMode::Heavy, RunVerdict::Fail(Verdict::Converged)) => serde_json::json!({
            "kind": "invalid_verdict",
            "start_another_campaign": false,
        }),
        (CampaignMode::Heavy, RunVerdict::Incomplete { .. }) => serde_json::json!({
            "kind": "resume_incomplete_round",
            "start_another_campaign": false,
        }),
    }
}

fn aggregate_usage(attempts: &[review_pipeline::AttemptEvidence]) -> review_runner::TokenUsage {
    fn sum(
        attempts: &[review_pipeline::AttemptEvidence],
        select: impl Fn(&review_runner::TokenUsage) -> Option<u64>,
    ) -> Option<u64> {
        attempts
            .iter()
            .filter_map(|attempt| select(&attempt.usage))
            .reduce(u64::saturating_add)
    }
    review_runner::TokenUsage {
        input_tokens: sum(attempts, |usage| usage.input_tokens),
        output_tokens: sum(attempts, |usage| usage.output_tokens),
        cache_read_tokens: sum(attempts, |usage| usage.cache_read_tokens),
        cache_write_tokens: sum(attempts, |usage| usage.cache_write_tokens),
        reasoning_tokens: sum(attempts, |usage| usage.reasoning_tokens),
        chargeable_tokens: attempts
            .iter()
            .map(|attempt| attempt.cost_tokens)
            .fold(0, u64::saturating_add),
    }
}

#[derive(Debug, serde::Serialize)]
struct AvailableNodeResult {
    node: String,
    attempt_id: String,
    result_artifact_id: String,
    severities: Vec<String>,
    spend_tokens: u64,
    findings: Vec<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct LatestRoundEvidence {
    ledger_production: &'static str,
    available_node_results: Vec<AvailableNodeResult>,
}

impl LatestRoundEvidence {
    fn ledger_was_not_produced(&self) -> bool {
        self.ledger_production.starts_with("not_produced_")
    }

    fn absence_reason(&self) -> &'static str {
        match self.ledger_production {
            "not_produced_upstream_missing" => "required upstream output was missing",
            "not_produced_failed" => "the Ledger node failed",
            "not_produced_gate_blocked" => "the Ledger node was gate-blocked",
            _ => "the Ledger node did not produce an authoritative output",
        }
    }
}

fn ledger_node_id(round_event: &review_core::RunEvent, cas: &Cas) -> Result<String, String> {
    let round: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(round_event.payload.clone()).map_err(|error| error.to_string())?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&round.campaign_manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    manifest.validate()?;
    let pipeline = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|error| error.to_string())?;
    let pipeline = std::str::from_utf8(&pipeline).map_err(|error| error.to_string())?;
    let definition =
        review_config::Definition::from_toml(pipeline).map_err(|error| error.to_string())?;
    let mut ledger_nodes = definition
        .nodes
        .iter()
        .filter(|node| node.kind == review_config::NodeKindSpec::Ledger);
    let node = ledger_nodes
        .next()
        .ok_or("pinned Campaign pipeline has no Ledger node")?;
    if ledger_nodes.next().is_some() {
        return Err("pinned Campaign pipeline has multiple Ledger nodes".into());
    }
    Ok(node.id.clone())
}

fn run_report_outcomes(
    event: &review_core::RunEvent,
) -> Result<Option<Vec<review_core::RunNodeReportV2>>, String> {
    match event.event_type {
        EventType::RunReportV2 => Ok(Some(
            serde_json::from_value::<RunReportPayloadV2>(event.payload.clone())
                .map_err(|error| error.to_string())?
                .outcomes,
        )),
        EventType::RunReportV3 => Ok(Some(
            serde_json::from_value::<RunReportPayloadV3>(event.payload.clone())
                .map_err(|error| error.to_string())?
                .outcomes,
        )),
        EventType::RunReportV4 => Ok(Some(
            serde_json::from_value::<RunReportPayloadV4>(event.payload.clone())
                .map_err(|error| error.to_string())?
                .outcomes,
        )),
        EventType::RunReportV5 => Ok(Some(
            serde_json::from_value::<RunReportPayloadV5>(event.payload.clone())
                .map_err(|error| error.to_string())?
                .outcomes,
        )),
        EventType::RunReportV1 => Ok(None),
        _ => Err(format!("{} is not a Run Report", event.event_type)),
    }
}

fn latest_round_evidence(
    events: &[review_core::RunEvent],
    cas: &Cas,
) -> Result<Option<LatestRoundEvidence>, String> {
    let Some(round_event) = events
        .iter()
        .rev()
        .find(|event| event.event_type == EventType::RoundStartedV1)
    else {
        return Ok(None);
    };
    let ledger_node_id = ledger_node_id(round_event, cas)?;
    let ledger_receipt = events.iter().rev().find(|event| {
        event.event_type == EventType::NodeOutputReceiptV1
            && event.node_id.as_deref() == Some(ledger_node_id.as_str())
            && event.causation_id.as_deref() == Some(round_event.event_id.as_str())
    });
    if let Some(receipt) = ledger_receipt {
        let receipt: review_core::NodeOutputReceiptPayloadV1 =
            serde_json::from_value(receipt.payload.clone()).map_err(|error| error.to_string())?;
        let mut count = None;
        for artifact_id in receipt
            .outputs
            .iter()
            .flat_map(|port| port.artifact_ids.iter())
        {
            let value = cas
                .get_json(artifact_id)
                .map_err(|error| error.to_string())?;
            let Ok(envelope) = serde_json::from_value::<review_core::ArtifactEnvelope>(value)
            else {
                continue;
            };
            if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
                continue;
            }
            review_store::validate_envelope(&envelope)?;
            let set: review_core::FindingSetV1 =
                serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
            set.validate()?;
            count = Some(set.findings.len());
            break;
        }
        return Ok(Some(LatestRoundEvidence {
            ledger_production: if count == Some(0) {
                "produced_clean"
            } else {
                "produced_with_findings"
            },
            available_node_results: Vec::new(),
        }));
    }

    let report = events.iter().rev().find(|event| {
        event.event_type.is_run_report()
            && event.causation_id.as_deref() == Some(round_event.event_id.as_str())
    });
    let Some(report) = report else {
        return Ok(None);
    };
    let Some(outcomes) = run_report_outcomes(report)? else {
        return Ok(None);
    };
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome.node == ledger_node_id)
        .ok_or("Run Report has no outcome for the pinned Ledger node")?;
    let ledger_production = match outcome.outcome {
        review_core::RunNodeOutcomeV2::Suppressed {
            reason: review_core::RunSuppressionReasonV2::UpstreamMissing,
        } => "not_produced_upstream_missing",
        review_core::RunNodeOutcomeV2::Suppressed {
            reason: review_core::RunSuppressionReasonV2::GateBlocked,
        } => "not_produced_gate_blocked",
        review_core::RunNodeOutcomeV2::Failed { .. } => "not_produced_failed",
        review_core::RunNodeOutcomeV2::Completed { .. } => {
            return Err("Ledger completed without a NodeOutputReceipt".into());
        }
    };
    let mut available = Vec::new();
    for event in events.iter().filter(|event| {
        event.event_type == EventType::AttemptAdmittedV1
            && event.causation_id.as_deref() == Some(round_event.event_id.as_str())
    }) {
        let payload: review_core::event::AttemptAdmittedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        if payload.selection != "selected" {
            continue;
        }
        let result_artifact_id = payload
            .result_artifact
            .ok_or("selected Attempt has no result artifact")?;
        let value = cas
            .get_json(&result_artifact_id)
            .map_err(|error| error.to_string())?;
        let reports = value
            .get("reports")
            .or_else(|| value.get("findings"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let findings = reports
            .into_iter()
            .map(|report| {
                serde_json::json!({
                    "severity": report.get("severity").cloned().unwrap_or(serde_json::Value::Null),
                    "title": report.get("title").cloned().unwrap_or(serde_json::Value::Null),
                    "body": report.get("body").cloned().unwrap_or(serde_json::Value::Null),
                    "file": report.get("file").cloned().unwrap_or(serde_json::Value::Null),
                    "line": report.get("line").cloned().unwrap_or(serde_json::Value::Null),
                    "locations": report.get("locations").cloned().unwrap_or(serde_json::Value::Null),
                })
            })
            .collect::<Vec<_>>();
        let severities = findings
            .iter()
            .filter_map(|finding| finding["severity"].as_str().map(str::to_string))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        available.push(AvailableNodeResult {
            node: event
                .node_id
                .clone()
                .ok_or("selected Attempt has no node ID")?,
            attempt_id: event
                .attempt_id
                .clone()
                .ok_or("selected Attempt has no Attempt ID")?,
            result_artifact_id,
            severities,
            spend_tokens: payload.cost_tokens,
            findings,
        });
    }
    available.sort_by(|left, right| {
        (&left.node, &left.attempt_id).cmp(&(&right.node, &right.attempt_id))
    });
    Ok(Some(LatestRoundEvidence {
        ledger_production,
        available_node_results: available,
    }))
}

fn packaged_runner(command: &review_core::Command) -> String {
    Path::new(&command.program)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `static_reservation_tokens` is every static Worker's first Attempt together — each node's
/// own cap where declared, the pipeline attempt cap elsewhere.
fn require_static_attempt_capacity(
    static_reservation_tokens: u64,
    run_tokens: u64,
    static_workers: usize,
    committed_tokens: u64,
) -> Result<(), String> {
    let required = committed_tokens
        .checked_add(static_reservation_tokens)
        .ok_or("Provider spend plus static Worker budget arithmetic overflow")?;
    if required > run_tokens {
        return Err(format!(
            "Provider admission committed {committed_tokens} tokens, leaving insufficient run budget for the first Attempt of each of {static_workers} required static Workers ({static_reservation_tokens} tokens reserved together): cap {run_tokens}, required {required}"
        ));
    }
    Ok(())
}

fn admit_review_providers(
    options: &Options,
    state: &Path,
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    loaded: &review_config::Loaded,
    authority: &RoundAuthority,
) -> Result<BTreeMap<String, providers::ProviderAdmission>, String> {
    for node in options.provider_bindings.keys() {
        if !loaded.reviewers().contains_key(node) {
            return Err(format!("--provider names unknown reviewer node `{node}`"));
        }
        if !loaded.packages().contains_key(node) {
            return Err(format!(
                "node `{node}` is an inline command; --provider is only valid for packaged Workers"
            ));
        }
    }
    for (node, package) in loaded.packages() {
        let runner = packaged_runner(&package.runner);
        if matches!(runner.as_str(), "claude" | "codex")
            && !options.provider_bindings.contains_key(node)
        {
            return Err(format!(
                "model-backed Worker `{node}` ({runner}) requires explicit `--provider {node}=PROVIDER_ID`; run `af provider doctor` with the same Campaign and selectors first"
            ));
        }
    }
    for node in store
        .provider_operation_nodes(run_id, authority.round_event_id())
        .map_err(|error| error.to_string())?
    {
        if !options.provider_bindings.contains_key(&node) {
            return Err(format!(
                "node `{node}` has Provider Admission state in this Round; repeat its explicit --provider binding"
            ));
        }
    }
    let expected_operations = options
        .provider_bindings
        .iter()
        .map(|(node, provider_id)| {
            let reviewer = loaded
                .reviewers()
                .get(node)
                .expect("provider binding node was validated");
            providers::operation_id_for(provider_id, node, reviewer, authority)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if let Some(stale) = options
        .provider_resumes
        .keys()
        .find(|operation| !expected_operations.contains(*operation))
    {
        return Err(format!(
            "--resume-provider `{stale}` is stale or does not belong to a configured provider operation"
        ));
    }
    let replayed_spend = store
        .round_committed_tokens(run_id, authority.round_event_id())
        .map_err(|error| error.to_string())?;
    let mut provider_budget = BudgetLedger::default().with_committed(Scope::Run, replayed_spend);
    if let Some(budgets) = loaded.budgets() {
        provider_budget = provider_budget.with_limit(Scope::Run, Budget::of(budgets.run));
    }
    let mut resumes = options.provider_resumes.clone();
    let mut structural_probes = BTreeSet::new();
    let mut admissions = BTreeMap::new();
    for (node, provider_id) in &options.provider_bindings {
        let command = loaded
            .reviewers()
            .get(node)
            .expect("provider binding node was validated");
        let admission = providers::admit(
            provider_id,
            providers::AdmissionRequest {
                node_id: node,
                reviewer: command,
                state_dir: state,
                run_id,
                authority,
                cas,
                store,
                resumes: &mut resumes,
                budget: &mut provider_budget,
                structural_probes: &mut structural_probes,
            },
        )?;
        admissions.insert(node.clone(), admission);
    }
    if let Some((operation, _)) = resumes.first_key_value() {
        return Err(format!(
            "--resume-provider `{operation}` is stale or does not belong to a configured provider operation"
        ));
    }
    let has_dispatched_worker = store
        .replay(run_id)
        .map_err(|error| error.to_string())?
        .iter()
        .any(|event| {
            event.event_type == EventType::AttemptDispatchedV1
                && event.causation_id.as_deref() == Some(authority.round_event_id())
        });
    if !has_dispatched_worker && let Some(budgets) = loaded.budgets() {
        let committed = store
            .round_committed_tokens(run_id, authority.round_event_id())
            .map_err(|error| error.to_string())?;
        let static_reservation = project::max_simultaneous_reservation(loaded)?.unwrap_or(0);
        require_static_attempt_capacity(
            static_reservation,
            budgets.run,
            loaded.reviewers().len(),
            committed,
        )?;
    }
    Ok(admissions)
}

fn provider_doctor(options: &Options) -> Result<(), String> {
    if options.campaign.is_none() {
        return Err("provider doctor requires `--campaign NAME` so admission evidence can be reused by review run".into());
    }
    let state = options.resolved_state_dir()?;
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|error| error.to_string())?;
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(&options.repo, &git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let authority::PreparedRun {
        loaded,
        run_id,
        authority,
        ..
    } = authority::prepare(options, &cas, &mut store, &repo)?;
    let admissions = admit_review_providers(
        options, &state, &cas, &mut store, &run_id, &loaded, &authority,
    )?;
    let spent = store
        .round_committed_tokens(&run_id, authority.round_event_id())
        .map_err(|error| error.to_string())?;
    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "schema": "af/provider-doctor@1",
                "ready": true,
                "run_id": run_id,
                "round": authority.round(),
                "epoch": authority.epoch(),
                "admitted_nodes": admissions.keys().collect::<Vec<_>>(),
                "committed_tokens": spent,
                "gates_run": false,
                "workers_dispatched": false,
            })
        );
    } else {
        println!("provider doctor: ready");
        for node in admissions.keys() {
            println!("  admitted {node}");
        }
        println!("  committed {spent} tokens; no Gates or Workers ran");
    }
    Ok(())
}

fn run(options: &Options) -> Result<RunVerdict, String> {
    let state = options.resolved_state_dir()?;
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|error| error.to_string())?;
    let home = std::env::var("HOME").map_err(|error| error.to_string())?;
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(&options.repo, &git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));

    let authority::PreparedRun {
        loaded,
        snapshot,
        run_id,
        focus,
        timeout,
        check_timeout,
        git_timeout,
        convergence,
        authority,
        ledger_projection,
    } = authority::prepare(options, &cas, &mut store, &repo)?;
    let authority_receipt = ReviewAuthority {
        authority_snapshot_id: authority.authority_snapshot_id().to_string(),
        campaign_manifest_id: authority.campaign_manifest_id().to_string(),
        subject_id: authority.subject_id().to_string(),
        head_snapshot_id: authority.head_snapshot_id().to_string(),
        round: authority.round(),
        epoch: authority.epoch(),
    };
    run_progress(
        options,
        format_args!(
            "mode     {} ({} clean, {} max Round{})",
            options.mode.as_str(),
            convergence.clean_rounds,
            convergence.max_rounds,
            if convergence.max_rounds == 1 { "" } else { "s" }
        ),
    );
    run_progress(options, format_args!("run      {run_id}"));
    run_progress(
        options,
        format_args!(
            "timeouts reviewer {}s, checks {}s, git capture {}s (pinned)",
            timeout.as_secs(),
            check_timeout.as_secs(),
            git_timeout.as_secs()
        ),
    );

    // Every model Worker's first-Attempt input, measured against the cap its dispatch reserves,
    // before any Gate, Provider admission, or Worker: an input that alone exhausts the cap is
    // refused here, with nothing charged, rather than after paying for the whole prompt.
    {
        let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
            cas.get_json(authority.campaign_manifest_id())
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let pipeline_bytes = cas
            .get(&manifest.pipeline.artifact_id)
            .map_err(|error| error.to_string())?;
        let definition = review_config::Definition::from_toml(
            std::str::from_utf8(&pipeline_bytes)
                .map_err(|error| format!("pinned pipeline is not UTF-8: {error}"))?,
        )
        .map_err(|error| error.to_string())?;
        let change_set = authority.change_set().map_or(
            authority::ChangeSetSource::None,
            authority::ChangeSetSource::Resolved,
        );
        let sizes = authority::worker_input_sizes(
            &cas,
            &definition,
            &loaded,
            change_set,
            focus.as_deref(),
        )?;
        for size in &sizes {
            run_progress(
                options,
                format_args!(
                    "input    {} {} bytes (about {} tokens){}",
                    size.node,
                    size.input_bytes,
                    size.input_tokens,
                    size.cap_tokens
                        .map(|cap| format!(" of a {cap}-token Attempt cap"))
                        .unwrap_or_default()
                ),
            );
        }
        authority::refuse_unfit_inputs(&sizes)?;
    }

    let admissions = admit_review_providers(
        options, &state, &cas, &mut store, &run_id, &loaded, &authority,
    )?;

    let auth = (std::env::var("USER").ok(), home.clone());
    let mut kernel = Kernel::from_loaded(&cas, &mut store, &run_id, snapshot, &loaded, authority)?
        .with_ledger_projection(ledger_projection)?
        .with_checks(loaded.checks().to_vec())
        .with_cache_source_resolver(caches::resolve_kind)
        .with_check_timeout(check_timeout);
    if let Some(budgets) = loaded.budgets() {
        run_progress(
            options,
            format_args!(
                "budgets  {} attempt reservation, {} run admission cap (chargeable tokens)",
                budgets.attempt, budgets.run
            ),
        );
        kernel = kernel.with_budgets(budgets.attempt, budgets.run);
    }

    let mut bound: BTreeMap<String, String> = BTreeMap::new();
    for (node, command) in loaded.reviewers() {
        let adapter: Box<dyn ReviewerAdapter> = match loaded.packages().get(node) {
            Some(package) => {
                let program = packaged_runner(command);
                match program.as_str() {
                    "claude" => {
                        let user = auth.0.clone().ok_or_else(|| {
                            format!("node `{node}`: Claude subscription auth requires USER")
                        })?;
                        let mut adapter =
                            review_runner_claude::ClaudeAdapter::from_package(package, timeout)
                                .map_err(|error| format!("{node}: {error}"))?
                                .with_auth(
                                    Some(
                                        admissions
                                            .get(node)
                                            .expect("model-backed package was explicitly admitted")
                                            .auth_dir_string()?,
                                    ),
                                    user,
                                    auth.1.clone(),
                                );
                        if let Some(focus) = &focus {
                            adapter = adapter.with_focus(focus);
                        }
                        Box::new(adapter)
                    }
                    "codex" => {
                        let codex_home = admissions
                            .get(node)
                            .expect("model-backed package was explicitly admitted")
                            .auth_dir_string()?;
                        let mut adapter =
                            review_runner_codex::CodexAdapter::from_package(package, timeout)
                                .map_err(|error| format!("{node}: {error}"))?
                                .with_codex_home(codex_home);
                        if let Some(focus) = &focus {
                            adapter = adapter.with_focus(focus);
                        }
                        Box::new(adapter)
                    }
                    other => {
                        return Err(format!(
                            "node `{node}`: no adapter drives `{other}`; this af release knows \
                             claude and codex"
                        ));
                    }
                }
            }
            None => Box::new(review_runner::CommandAdapter::new(command.clone(), timeout)),
        };
        bound.insert(node.clone(), command.program.clone());
        kernel = kernel.with_adapter(node.clone(), adapter);
    }
    for (node, program) in &bound {
        run_progress(options, format_args!("reviewer {node} -> {program}"));
    }

    let report = loaded.run(&kernel).map_err(|error| error.to_string())?;
    run_progress(options, format_args!(""));
    for (node, outcome) in &report.outcomes {
        match outcome {
            NodeOutcome::Completed { .. } => {
                run_progress(options, format_args!("  done      {node}"))
            }
            NodeOutcome::Failed { error, .. } => {
                run_progress(options, format_args!("  FAILED    {node}: {error}"))
            }
            NodeOutcome::Suppressed { reason } => {
                run_progress(options, format_args!("  never-ran {node}: {reason:?}"))
            }
        }
    }

    let ledger = kernel.ledger();
    print_scope_authority_warnings(&ledger);
    run_progress(options, format_args!(""));
    let finding_views = ledger.finding_views();
    run_progress(options, format_args!("findings {}", finding_views.len()));
    for finding in finding_views {
        run_progress(
            options,
            format_args!(
                "  [{:?}] {}:{} - {} ({:?})",
                finding.severity,
                finding.file,
                finding
                    .line
                    .map_or("?".to_string(), |line| line.to_string()),
                finding.title,
                finding.status
            ),
        );
    }
    let open_or_stale_demand_ids = ledger
        .demand_views()
        .into_iter()
        .filter(|demand| {
            demand.requirement == review_core::DemandRequirement::Required
                && matches!(
                    demand.status,
                    review_core::DemandStatus::Open | review_core::DemandStatus::Stale
                )
        })
        .map(|demand| demand.demand_id)
        .collect::<Vec<_>>();
    run_progress(
        options,
        format_args!(
            "demands  {} open/stale (required)",
            open_or_stale_demand_ids.len()
        ),
    );
    let spent_tokens = kernel.spent();
    if let Some(spent) = spent_tokens {
        run_progress(options, format_args!("spent    {spent} tokens"));
    }

    let verdict = kernel.publish_report(&report, convergence)?;
    let attempts = kernel.selected_attempt_evidence()?;
    drop(kernel);
    let events = store.replay(&run_id).map_err(|error| error.to_string())?;
    let latest_evidence = latest_round_evidence(&events, &cas)?;
    if !options.json
        && let Some(evidence) = latest_evidence.as_ref()
        && evidence.ledger_was_not_produced()
        && !evidence.available_node_results.is_empty()
    {
        run_progress(options, format_args!(""));
        run_progress(
            options,
            format_args!("recorded, not gathered (Ledger was not produced):"),
        );
        for result in &evidence.available_node_results {
            run_progress(
                options,
                format_args!(
                    "  {} attempt {} artifact {} spend {} severities [{}]",
                    result.node,
                    result.attempt_id,
                    result.result_artifact_id,
                    result.spend_tokens,
                    result.severities.join(", ")
                ),
            );
            for finding in &result.findings {
                run_progress(
                    options,
                    format_args!(
                        "    [{}] {}",
                        finding["severity"].as_str().unwrap_or("unknown"),
                        finding["title"].as_str().unwrap_or("untitled finding")
                    ),
                );
            }
        }
    }
    if options.json {
        let candidate = candidate_identity()?;
        let findings = ledger
            .finding_views()
            .into_iter()
            .map(|finding| {
                serde_json::json!({
                    "key": finding.key,
                    "severity": format!("{:?}", finding.severity).to_lowercase(),
                    "effective_severity": finding.convergence_severity
                        .map(|severity| format!("{severity:?}").to_lowercase()),
                    "status": finding.status.as_str(),
                    "scope": finding.convergence_scope_label(),
                    "file": finding.file,
                    "line": finding.line,
                    "title": finding.title,
                    "aliases": finding.aliases,
                })
            })
            .collect::<Vec<_>>();
        let node_outcomes = report
            .outcomes
            .iter()
            .map(|(node, outcome)| match outcome {
                NodeOutcome::Completed { outputs } => serde_json::json!({
                    "node": node,
                    "kind": "completed",
                    "output_artifacts": outputs.values().flatten().collect::<Vec<_>>(),
                }),
                NodeOutcome::Failed { error, .. } => serde_json::json!({
                    "node": node,
                    "kind": "failed",
                    "error": error,
                }),
                NodeOutcome::Suppressed { reason } => serde_json::json!({
                    "node": node,
                    "kind": "suppressed",
                    "reason": format!("{reason:?}").to_lowercase(),
                }),
            })
            .collect::<Vec<_>>();
        let rendered_bytes = attempts
            .iter()
            .map(|attempt| attempt.context_manifest.rendered_bytes)
            .fold(0, u64::saturating_add);
        let estimated_tokens = attempts
            .iter()
            .map(|attempt| attempt.context_manifest.estimated_tokens)
            .fold(0, u64::saturating_add);
        let usage = aggregate_usage(&attempts);
        let attempt_values = attempts
            .iter()
            .map(|attempt| {
                serde_json::json!({
                    "node": &attempt.node,
                    "attempt_id": &attempt.attempt_id,
                    "cost_tokens": attempt.cost_tokens,
                    "usage": &attempt.usage,
                    "context_manifest": &attempt.context_manifest,
                    "raw_artifact": &attempt.raw_artifact,
                    "result_artifact": &attempt.result_artifact,
                })
            })
            .collect::<Vec<_>>();
        let mut outcome_value = serde_json::json!({
            "schema": "af/review-outcome@1",
            "campaign_mode": options.mode.as_str(),
            "candidate": {
                "version": candidate.version,
                "executable": candidate.executable,
                "binary_sha256": candidate.binary_sha256,
            },
            "run_id": run_id,
            "authority": {
                "authority_snapshot_id": authority_receipt.authority_snapshot_id,
                "campaign_manifest_id": authority_receipt.campaign_manifest_id,
                "subject_id": authority_receipt.subject_id,
                "head_snapshot_id": authority_receipt.head_snapshot_id,
                "round": authority_receipt.round,
                "epoch": authority_receipt.epoch,
            },
            "node_outcomes": node_outcomes,
            "blocked_gates": report.blocked_gates,
            "attempts": attempt_values,
            "totals": {
                "context": {
                    "rendered_bytes": rendered_bytes,
                    "estimated_tokens": estimated_tokens,
                },
                "usage": usage,
                "spent_tokens": spent_tokens,
                "open_required_demands": open_or_stale_demand_ids.len(),
                "open_or_stale_demand_ids": open_or_stale_demand_ids,
            },
            "findings": findings,
            "outcome": verdict_value(&verdict),
            "next_action": next_action_value(options.mode, &verdict),
        });
        if let Some(evidence) = latest_evidence
            && evidence.ledger_was_not_produced()
        {
            let object = outcome_value.as_object_mut().expect("outcome object");
            object.insert(
                "ledger_production".into(),
                serde_json::Value::String(evidence.ledger_production.into()),
            );
            object.insert(
                "available_node_results".into(),
                serde_json::to_value(evidence.available_node_results)
                    .map_err(|error| error.to_string())?,
            );
        }
        println!(
            "{}",
            serde_json::to_string(&outcome_value).map_err(|error| error.to_string())?
        );
    } else {
        run_progress(options, format_args!("verdict  {verdict:?}"));
    }
    match (options.mode, &verdict) {
        (CampaignMode::Light, RunVerdict::Fail(_)) => run_progress(
            options,
            format_args!(
                "next     fix the findings, run the deterministic project gate, then stop; do not start another Campaign (use --heavy only by explicit human choice)"
            ),
        ),
        (CampaignMode::Light, RunVerdict::Incomplete { .. }) => run_progress(
            options,
            format_args!("next     resume this exact incomplete light Round"),
        ),
        _ => {}
    }
    Ok(verdict)
}

#[cfg(test)]
mod option_tests {
    use super::{
        CampaignMode, Options, campaign_id, campaign_state_beneath, enumerate_campaigns,
        last_closed_summary, latest_round_evidence, report_round_authority, report_rounds,
        report_spend, require_static_attempt_capacity, validate_campaign_name,
    };

    fn event(
        sequence: u64,
        event_type: review_core::EventType,
        causation_id: Option<&str>,
        node_id: Option<&str>,
        attempt_id: Option<&str>,
        payload: serde_json::Value,
    ) -> review_core::RunEvent {
        review_core::RunEvent {
            event_id: format!("event-{sequence}"),
            run_id: "campaign-partial".into(),
            sequence,
            event_type,
            occurred_at: "2026-08-28T00:00:00Z".into(),
            node_id: node_id.map(str::to_string),
            attempt_id: attempt_id.map(str::to_string),
            causation_id: causation_id.map(str::to_string),
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload,
        }
    }

    fn campaign_manifest(cas: &review_store::Cas, ledger_node: &str) -> (String, String, String) {
        let pipeline = format!(
            "version = 2\n[subject]\nkind = \"whole-tree\"\n[[nodes]]\nid = \"{ledger_node}\"\nkind = \"ledger\"\n"
        );
        let pipeline_id = cas.put(pipeline.as_bytes()).unwrap();
        let opaque = cas.put(b"pinned authority").unwrap();
        let findings = cas.put(b"finding genesis").unwrap();
        let demands = cas.put(b"demand genesis").unwrap();
        let manifest = review_core::CampaignManifestV1 {
            authority_snapshot_id: opaque.clone(),
            subject_kind: review_core::SubjectKind::WholeTree,
            base_snapshot_id: None,
            pipeline: review_core::AuthorityFileV1 {
                path: ".af/pipelines/review.toml".into(),
                artifact_id: pipeline_id,
            },
            reviewer_lock: review_core::AuthorityFileV1 {
                path: ".af/review.lock".into(),
                artifact_id: opaque,
            },
            reviewers: Vec::new(),
            execution_policy_ids: Vec::new(),
            project_policy_ids: Vec::new(),
            convergence: review_core::CampaignConvergenceV1 {
                clean_rounds: 1,
                max_rounds: 2,
                gate: "major".into(),
            },
            reviewer_timeout_seconds: 60,
            check_timeout_seconds: None,
            git_timeout_seconds: None,
            budgets: None,
            focus: None,
            finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            finding_genesis_id: findings.clone(),
            demand_genesis_id: demands.clone(),
        };
        manifest.validate().unwrap();
        let manifest_id = cas
            .put_json(&serde_json::to_value(manifest).unwrap())
            .unwrap();
        (manifest_id, findings, demands)
    }

    fn write_campaign_opening(state: &std::path::Path, label: &str) {
        std::fs::create_dir_all(state).unwrap();
        let cas = review_store::Cas::open(state.join("cas")).unwrap();
        let (manifest_id, _, _) = campaign_manifest(&cas, "ledger");
        let manifest: review_core::CampaignManifestV1 =
            serde_json::from_value(cas.get_json(&manifest_id).unwrap()).unwrap();
        let mut store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
        store
            .append(
                &super::campaign_run_id(label),
                &cas,
                review_store::NewEvent::new(
                    review_core::EventType::CampaignOpenedV1,
                    serde_json::to_value(review_core::CampaignOpenedPayloadV1 {
                        campaign_manifest_id: manifest_id.clone(),
                        authority_snapshot_id: manifest.authority_snapshot_id.clone(),
                    })
                    .unwrap(),
                )
                .referencing(vec![manifest.authority_snapshot_id, manifest_id]),
            )
            .unwrap();
    }

    #[test]
    fn campaign_names_cannot_redirect_state() {
        for invalid in [
            "",
            "../reviewers/architecture",
            "nested/name",
            "nested\\name",
            ".",
            "..",
            "control\nname",
            " padded",
            "padded ",
        ] {
            assert!(validate_campaign_name(invalid).is_err());
        }
        for valid in ["heavy", "reviewctl-tui", "round_4", "v2.1-audit"] {
            assert!(validate_campaign_name(valid).is_ok());
        }
    }

    #[test]
    fn campaign_state_uses_an_opaque_contained_id_and_legacy_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaigns");
        std::fs::create_dir_all(&root).unwrap();
        let id = campaign_id("heavy");
        assert_eq!(id.len(), 66);
        assert!(id.starts_with("c-"));
        assert!(id[2..].bytes().all(|byte| byte.is_ascii_hexdigit()));

        let encoded = campaign_state_beneath(&root, "heavy").unwrap();
        assert_eq!(encoded.file_name().unwrap(), id.as_str());
        assert!(encoded.starts_with(std::fs::canonicalize(&root).unwrap()));

        let legacy = root.join("heavy");
        write_campaign_opening(&legacy, "heavy");
        assert_eq!(
            campaign_state_beneath(&root, "heavy").unwrap(),
            std::fs::canonicalize(&legacy).unwrap()
        );

        std::fs::create_dir(&encoded).unwrap();
        assert!(
            campaign_state_beneath(&root, "heavy")
                .unwrap_err()
                .contains("both encoded and legacy")
        );
        let Err(error) = enumerate_campaigns(&root) else {
            panic!("ambiguous Campaign state was enumerated");
        };
        assert!(error.contains("both encoded and legacy"));
        assert!(
            campaign_state_beneath(&root, &id)
                .unwrap_err()
                .contains("reserved opaque Campaign ID shape")
        );
    }

    #[test]
    fn preexisting_legacy_labels_remain_readable_and_enumerable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaigns");
        std::fs::create_dir(&root).unwrap();
        let legacy = root.join(" padded");
        write_campaign_opening(&legacy, " padded");

        assert_eq!(
            campaign_state_beneath(&root, " padded").unwrap(),
            std::fs::canonicalize(&legacy).unwrap()
        );
        assert_eq!(
            super::campaign_state(&Some(legacy.clone()), " padded").unwrap(),
            std::fs::canonicalize(&legacy).unwrap()
        );
        let enumeration = enumerate_campaigns(&root).unwrap();
        assert_eq!(enumeration.campaigns.len(), 1);
        assert_eq!(enumeration.campaigns[0].label, " padded");
        assert!(enumeration.problems.is_empty());
    }

    #[test]
    fn campaign_enumeration_does_not_create_a_missing_cas() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaigns");
        let state = root.join("legacy");
        write_campaign_opening(&state, "legacy");
        std::fs::remove_dir_all(state.join("cas")).unwrap();

        let enumeration = enumerate_campaigns(&root).unwrap();
        assert!(enumeration.campaigns.is_empty());
        assert_eq!(enumeration.problems.len(), 1);
        assert!(!state.join("cas").exists());
    }

    #[cfg(unix)]
    #[test]
    fn bad_entries_do_not_hide_campaigns_and_symlinked_state_is_not_followed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaigns");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        write_campaign_opening(&root.join("good"), "good");
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();

        let enumeration = enumerate_campaigns(&root).unwrap();
        assert_eq!(enumeration.campaigns.len(), 1);
        assert_eq!(enumeration.campaigns[0].label, "good");
        assert_eq!(enumeration.problems.len(), 1);
        assert!(enumeration.problems[0].reason.contains("symlink"));
    }

    #[test]
    fn unreadable_legacy_sibling_is_attributed_and_blocks_a_false_healthy_listing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaigns");
        std::fs::create_dir(&root).unwrap();
        let encoded = root.join(campaign_id("healthy"));
        write_campaign_opening(&encoded, "healthy");
        let legacy = root.join("healthy");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("events.sqlite"), b"not sqlite").unwrap();

        let error = campaign_state_beneath(&root, "healthy").unwrap_err();
        assert!(error.contains("legacy Campaign state"), "{error}");
        assert!(error.contains("events.sqlite"), "{error}");

        let enumeration = enumerate_campaigns(&root).unwrap();
        assert!(enumeration.campaigns.is_empty());
        assert!(
            enumeration
                .problems
                .iter()
                .any(|problem| problem.reason.contains("legacy sibling state"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn campaign_enumeration_refuses_a_symlinked_event_database() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaigns");
        let outside = temp.path().join("outside");
        let state = root.join("linked");
        write_campaign_opening(&outside, "linked");
        std::fs::create_dir_all(&state).unwrap();
        std::os::unix::fs::symlink(outside.join("events.sqlite"), state.join("events.sqlite"))
            .unwrap();

        let enumeration = enumerate_campaigns(&root).unwrap();
        assert!(enumeration.campaigns.is_empty());
        assert_eq!(enumeration.problems.len(), 1);
        assert!(enumeration.problems[0].reason.contains("real regular file"));
    }

    #[test]
    fn spend_projection_keeps_fenced_released_and_outstanding_work_visible() {
        let round = event(
            0,
            review_core::EventType::RoundStartedV1,
            None,
            None,
            None,
            serde_json::json!({
                "round": 1,
                "epoch": 1,
                "campaign_manifest_id": "manifest",
                "subject_id": "subject",
                "prior_finding_set_id": "findings",
                "prior_demand_set_id": "demands"
            }),
        );
        let attempt = |sequence, event_type, node, attempt_id, payload| {
            event(
                sequence,
                event_type,
                Some("event-0"),
                Some(node),
                Some(attempt_id),
                payload,
            )
        };
        let events = vec![
            round,
            attempt(
                1,
                review_core::EventType::AttemptDispatchedV1,
                "architecture",
                "selected",
                serde_json::json!({"reserved": 100, "prior_findings": null}),
            ),
            attempt(
                2,
                review_core::EventType::AttemptAdmittedV1,
                "architecture",
                "selected",
                serde_json::json!({
                    "selection": "selected",
                    "cost_tokens": 31,
                    "result_artifact": null,
                    "provenance_artifact": null
                }),
            ),
            attempt(
                3,
                review_core::EventType::AttemptDispatchedV1,
                "architecture",
                "fenced",
                serde_json::json!({"reserved": 50, "prior_findings": null}),
            ),
            attempt(
                4,
                review_core::EventType::AttemptFencedV1,
                "architecture",
                "fenced",
                serde_json::json!({"reason": "deadline", "charged": 11}),
            ),
            attempt(
                5,
                review_core::EventType::AttemptAdmittedV1,
                "architecture",
                "fenced",
                serde_json::json!({
                    "selection": "quarantined",
                    "cost_tokens": 11,
                    "result_artifact": null,
                    "provenance_artifact": null
                }),
            ),
            attempt(
                6,
                review_core::EventType::AttemptDispatchedV1,
                "correctness",
                "released",
                serde_json::json!({"reserved": 20, "prior_findings": null}),
            ),
            attempt(
                7,
                review_core::EventType::AttemptReleasedV1,
                "correctness",
                "released",
                serde_json::json!({"error": "not run", "released": 20}),
            ),
            attempt(
                8,
                review_core::EventType::AttemptDispatchedV1,
                "correctness",
                "running",
                serde_json::json!({"reserved": 7, "prior_findings": null}),
            ),
            event(
                9,
                review_core::EventType::ProviderOperationTransitionV1,
                Some("event-0"),
                Some("architecture"),
                None,
                serde_json::json!({
                    "operation_id": "provider-op",
                    "provider_id": "codex",
                    "capability_id": "smoke",
                    "node_id": "architecture",
                    "round": 1,
                    "round_epoch": 1,
                    "operation_epoch": 1,
                    "state": "failed",
                    "attempt": null,
                    "attempt_id": null,
                    "failure_class": "transient_transport_failure",
                    "failure_fingerprint": "fingerprint",
                    "continuation_handle": null,
                    "reserved_tokens": 5,
                    "charged_tokens": 2,
                    "elapsed_ms": 10,
                    "retry_permitted": true,
                    "circuit_open": false,
                    "next_action": "retry_explicitly"
                }),
            ),
            attempt(
                10,
                review_core::EventType::AttemptDispatchedV1,
                "correctness",
                "brokered",
                serde_json::json!({"reserved": null, "prior_findings": null}),
            ),
            attempt(
                11,
                review_core::EventType::ReviewerExecutionBoundV1,
                "correctness",
                "brokered",
                serde_json::json!({
                    "node": "correctness",
                    "attempt_id": "brokered",
                    "lease_epoch": 1,
                    "credential_mode": "brokered",
                    "auto_apply": false,
                    "broker_handle": "bbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "operations": [{
                        "name": "model_inference",
                        "destination": "provider.test",
                        "method": "responses.create",
                        "max_request_bytes": 32,
                        "max_response_bytes": 32,
                        "max_calls": 1,
                        "max_usage": 100
                    }],
                    "admitted": true
                }),
            ),
            attempt(
                12,
                review_core::EventType::AttemptFencedV1,
                "correctness",
                "brokered",
                serde_json::json!({"reason": "recovery", "charged": 100}),
            ),
            attempt(
                13,
                review_core::EventType::BrokerOperationCompletedV1,
                "correctness",
                "brokered",
                serde_json::json!({
                    "handle_id": "bbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "node": "correctness",
                    "attempt_id": "brokered",
                    "lease_epoch": 1,
                    "operation": "model_inference",
                    "destination": "provider.test",
                    "method": "responses.create",
                    "ordinal": 1,
                    "outcome": "revoked",
                    "failure_reason": "authority_revoked",
                    "request_digest": format!("sha256:{}", "c".repeat(64)),
                    "request_bytes": 7,
                    "response_bytes": 0,
                    "reserved_usage": 100,
                    "charged_usage": 101
                }),
            ),
        ];

        let authority = report_round_authority(&events).unwrap();
        let spend = report_spend(&events, &authority).unwrap();
        assert_eq!(spend.len(), 1);
        assert_eq!(spend[0].spent_tokens, 152);
        let architecture = &spend[0].reviewers[0];
        assert_eq!(architecture.reviewer, "architecture");
        assert_eq!(architecture.attempt_tokens, 42);
        assert_eq!(architecture.provider_tokens, 2);
        let fenced = architecture
            .attempts
            .iter()
            .find(|attempt| attempt.attempt_id == "fenced")
            .unwrap();
        assert_eq!(fenced.outcome, "fenced");
        assert_eq!(fenced.spent_tokens, 11);
        let correctness = &spend[0].reviewers[1];
        assert_eq!(correctness.attempt_tokens, 108);
        let attempt = |id| {
            correctness
                .attempts
                .iter()
                .find(|attempt| attempt.attempt_id == id)
                .unwrap()
        };
        assert_eq!(attempt("released").outcome, "released");
        assert_eq!(attempt("released").spent_tokens, 0);
        assert_eq!(attempt("running").outcome, "running");
        assert_eq!(attempt("running").spent_tokens, 7);
        assert_eq!(attempt("brokered").outcome, "fenced");
        assert_eq!(attempt("brokered").spent_tokens, 101);
    }

    #[test]
    fn legacy_report_without_round_authority_remains_reportable() {
        let legacy = event(
            0,
            review_core::EventType::RunReportV1,
            None,
            None,
            None,
            serde_json::json!({
                "outcomes": [{"node": "reviewer", "status": "completed", "detail": {}}],
                "blocked_gates": [],
                "verdict": "Pass",
                "spent_tokens": 9
            }),
        );
        let rounds = report_rounds(&[&legacy], &std::collections::BTreeMap::new()).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].run, 1);
        assert_eq!(rounds[0].round, None);
        assert_eq!(rounds[0].epoch, None);
        assert_eq!(rounds[0].verdict, "Pass");
        assert_eq!(rounds[0].reported_tokens, Some(9));
        assert_eq!(
            last_closed_summary(rounds[0].round, rounds[0].epoch, Some(&rounds[0].verdict)),
            Some("round - epoch -; Pass".to_string())
        );
    }

    #[test]
    fn repository_state_is_always_refused() {
        let repository = tempfile::tempdir().unwrap();
        let options = Options {
            repo: repository.path().to_path_buf(),
            pipeline: ".af/pipelines/review.toml".into(),
            pipeline_explicit: false,
            state: Some(repository.path().join(".af/state/architecture")),
            campaign: Some("architecture".to_string()),
            focus: None,
            policy_rev: None,
            base: None,
            candidate: None,
            authority: None,
            uncommitted: false,
            restart_round: false,
            mode: CampaignMode::Light,
            timeout: None,
            git_timeout: None,
            provider_bindings: std::collections::BTreeMap::new(),
            provider_resumes: std::collections::BTreeMap::new(),
            json: false,
            node: None,
        };
        let error = options.resolved_state_dir().unwrap_err();
        assert!(error.contains("state must live under XDG state"));
    }

    #[test]
    fn provider_smoke_spend_cannot_consume_static_attempt_capacity() {
        let error = require_static_attempt_capacity(600_000, 600_000, 2, 2).unwrap_err();
        assert!(error.contains("cap 600000, required 600002"), "{error}");
        require_static_attempt_capacity(600_000, 600_002, 2, 2).unwrap();
    }

    #[test]
    fn admitted_result_is_visible_without_becoming_a_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let cas = review_store::Cas::open(temp.path()).unwrap();
        let (manifest_id, findings_id, demands_id) = campaign_manifest(&cas, "reduce");
        let result_id = cas
            .put_json(&serde_json::json!({
                "verdict": "request-changes",
                "summary": null,
                "reports": [{
                    "severity": "major",
                    "file": "src/lib.rs",
                    "line": 9,
                    "title": "partial finding",
                    "body": "the sibling reviewer failed before gather",
                    "fix": "repair it",
                    "confidence": 0.9
                }],
                "benchmark_demands": [],
                "dispositions": []
            }))
            .unwrap();
        let events = vec![
            event(
                0,
                review_core::EventType::RoundStartedV1,
                None,
                None,
                None,
                serde_json::to_value(review_core::RoundStartedPayloadV1 {
                    round: 1,
                    epoch: 1,
                    campaign_manifest_id: manifest_id,
                    subject_id: findings_id.clone(),
                    prior_finding_set_id: findings_id,
                    prior_demand_set_id: demands_id,
                })
                .unwrap(),
            ),
            event(
                1,
                review_core::EventType::AttemptAdmittedV1,
                Some("event-0"),
                Some("correctness"),
                Some("attempt-1"),
                serde_json::json!({
                    "selection": "selected",
                    "cost_tokens": 37,
                    "result_artifact": result_id,
                    "provenance_artifact": null
                }),
            ),
            event(
                2,
                review_core::EventType::RunReportV3,
                Some("event-0"),
                None,
                None,
                serde_json::to_value(review_core::RunReportPayloadV3 {
                    outcomes: vec![review_core::RunNodeReportV2 {
                        node: "reduce".into(),
                        outcome: review_core::RunNodeOutcomeV2::Suppressed {
                            reason: review_core::RunSuppressionReasonV2::UpstreamMissing,
                        },
                    }],
                    blocked_gates: Vec::new(),
                    verdict: review_core::RunVerdictV3::Incomplete {
                        missing_nodes: Vec::new(),
                    },
                    spent_tokens: Some(37),
                })
                .unwrap(),
            ),
        ];
        let evidence = latest_round_evidence(&events, &cas).unwrap().unwrap();
        assert_eq!(evidence.ledger_production, "not_produced_upstream_missing");
        assert_eq!(evidence.available_node_results.len(), 1);
        let result = &evidence.available_node_results[0];
        assert_eq!(result.node, "correctness");
        assert_eq!(result.attempt_id, "attempt-1");
        assert_eq!(result.spend_tokens, 37);
        assert_eq!(result.severities, ["major"]);
        assert_eq!(result.findings[0]["title"], "partial finding");

        let mut gathered = events.clone();
        gathered.insert(
            2,
            event(
                2,
                review_core::EventType::NodeOutputReceiptV1,
                Some("event-0"),
                Some("reduce"),
                None,
                serde_json::json!({"node": "reduce", "outputs": []}),
            ),
        );
        let evidence = latest_round_evidence(&gathered, &cas).unwrap().unwrap();
        assert_eq!(evidence.ledger_production, "produced_with_findings");
        assert!(evidence.available_node_results.is_empty());

        let mut failed = events;
        failed.last_mut().unwrap().payload =
            serde_json::to_value(review_core::RunReportPayloadV3 {
                outcomes: vec![review_core::RunNodeReportV2 {
                    node: "reduce".into(),
                    outcome: review_core::RunNodeOutcomeV2::Failed {
                        error: "invalid Ledger output".into(),
                    },
                }],
                blocked_gates: Vec::new(),
                verdict: review_core::RunVerdictV3::Incomplete {
                    missing_nodes: Vec::new(),
                },
                spent_tokens: Some(37),
            })
            .unwrap();
        let evidence = latest_round_evidence(&failed, &cas).unwrap().unwrap();
        assert_eq!(evidence.ledger_production, "not_produced_failed");
    }
}
