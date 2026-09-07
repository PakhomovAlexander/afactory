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

mod authority;
mod caches;
mod cli;
mod config;
mod onboard;
mod project;
mod providers;
mod review;
mod selfmgmt;
mod task;
mod topics;
mod tui;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use review_core::Severity;
use review_pipeline::RunVerdict;
use review_store::EventStore;
use sha2::{Digest, Sha256};

use review::campaigns::{enumerate_campaigns, print_campaigns, print_gc};
use review::ledger::{
    add_evidence, advance_policy_time, attest_change, challenge_resolution, export_proposal, group,
    print_ledger, resolve, satisfy_evidence, show, verify_fix, waive_demand,
};
use review::report::print_report;
use review::run::{print_plan, print_render, provider_doctor, run};

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

/// The one opaque name a canonical repository has beneath every per-repository local state
/// root (`af/review/local`, `af/task/local`). The bytes are frozen: existing state directories
/// are addressed by exactly this undomained SHA-256 prefix, so it stays distinct from the
/// domain-separated Campaign ID (ADR-0035) rather than being unified with it.
fn repository_state_id(canonical_repository: &Path) -> String {
    let identity = Sha256::digest(canonical_repository.as_os_str().as_encoded_bytes());
    format!("{identity:x}")[..16].to_string()
}

fn default_local_state(repository: &Path) -> Result<PathBuf, String> {
    let repository = std::fs::canonicalize(repository)
        .map_err(|error| format!("opening repository {}: {error}", repository.display()))?;
    Ok(xdg_state_root()?
        .join("af/review/local")
        .join(repository_state_id(&repository)))
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
    /// Walk every state directory for its size; costs seconds on large roots, so opt-in.
    sizes: bool,
}

struct GcOptions {
    state_root: Option<PathBuf>,
    /// Campaigns whose newest store write is at least this many days old are candidates.
    older_than_days: Option<u64>,
    /// The newest N campaigns (by activity) are never candidates.
    keep: Option<usize>,
    apply: bool,
    json: bool,
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
    enumerate_campaigns(&root, false)
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
        R::Gc {
            state_root,
            older_than,
            keep,
            apply,
            json,
        } => print_gc(&GcOptions {
            state_root,
            older_than_days: older_than,
            keep,
            apply,
            json,
        })
        .map(|()| 0),
        R::Campaigns {
            state_root,
            format,
            sizes,
        } => print_campaigns(&CampaignsOptions {
            state_root,
            sizes,
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

struct CandidateIdentity {
    version: &'static str,
    executable: String,
    binary_sha256: String,
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

#[cfg(test)]
mod option_tests {
    use super::review::campaigns::enumerate_campaigns;
    use super::review::evidence::latest_round_evidence;
    use super::review::report::{
        last_closed_summary, report_round_authority, report_rounds, report_spend,
    };
    use super::review::run::require_static_attempt_capacity;
    use super::{
        CampaignMode, Options, campaign_id, campaign_state_beneath, repository_state_id,
        validate_campaign_name,
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
        // Pinned authority replays through the shape the loader admitted, so the fixture is
        // a pipeline the loader would admit: a review needs a reviewer.
        let pipeline = format!(
            "version = 2\n[subject]\nkind = \"whole-tree\"\n[[nodes]]\nid = \"reviewer\"\nkind = \"reviewer\"\nrunner = {{ program = \"/bin/true\" }}\n[[nodes]]\nid = \"{ledger_node}\"\nkind = \"ledger\"\n"
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
    fn repository_state_id_is_the_frozen_undomained_digest_prefix() {
        // Pinned bytes: review and Task state directories already on disk are named by this
        // exact value, so the shared helper must keep producing it for the same canonical path.
        let id = repository_state_id(std::path::Path::new("/srv/repos/afactory"));
        assert_eq!(id, "da65312a575a5687");
        assert_eq!(id.len(), 16);
        assert_ne!(
            repository_state_id(std::path::Path::new("/srv/repos/afactory/")),
            id,
            "the digest covers the exact path bytes; callers canonicalize first"
        );
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
        let Err(error) = enumerate_campaigns(&root, false) else {
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
        let enumeration = enumerate_campaigns(&root, false).unwrap();
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

        let enumeration = enumerate_campaigns(&root, false).unwrap();
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

        let enumeration = enumerate_campaigns(&root, false).unwrap();
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

        let enumeration = enumerate_campaigns(&root, false).unwrap();
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

        let enumeration = enumerate_campaigns(&root, false).unwrap();
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
