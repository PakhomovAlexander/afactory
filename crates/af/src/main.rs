//! The `af` binary: it parses the command tree defined in `cli`, dispatches every namespace, and
//! implements the `af review` Campaign loop itself.
//!
//! The review namespace's core subcommands:
//!
//! - `run` captures the candidate as an immutable snapshot, pins the pipeline through its
//!   lockfile, captures the Campaign's Review Task, and executes it on the common Task runtime
//!   (`review_task`) under the captured budgets, printing every node, finding, charge and the
//!   verdict. With `--campaign NAME` later runs resume that Task, and a heavy Campaign's next
//!   Round gives every reviewer the Campaign's prior findings as a labelled data artifact.
//! - `ledger` prints a campaign's findings, one per line, machine-readably.
//! - `resolve` records the operator's non-fixed disposition of one finding (rejected or
//!   wontfix-tracked) in the campaign's ledger. A finding becomes fixed only through
//!   `attest-change` and a `verify-fix` against the current Subject.
//! - `group` and `ungroup` append reversible adjudication between duplicate Findings without
//!   erasing either identity, Report history, or verification obligation.
//!
//! The review loop never mutates a repository. A run reads a repo and writes its own state
//! directory; `resolve` writes only that state; publishing results anywhere is a human's
//! explicit action.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use review_core::{EventType, RunFailureReasonV3, RunReportPayloadV6, RunVerdictV3, Severity};
use review_pipeline::RunVerdict;
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
mod report_tasks;
mod review_task;
mod self_optimizer;
mod selfmgmt;
mod task;
mod task_execution;
mod topics;

use review_config::captured_review::ReviewMode as CampaignMode;

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
    uncommitted: bool,
    restart_round: bool,
    mode: CampaignMode,
    timeout: Option<Duration>,
    git_timeout: Option<Duration>,
    provider_bindings: BTreeMap<String, String>,
    provider_admission: Option<review_graph::task::OperatorAttemptCost>,
    json: bool,
    /// `af review render`: the Worker whose exact input to compose.
    node: Option<String>,
}

impl Options {
    /// Resolve state once for both execution and presentation. Relative paths use the process
    /// working directory, and state inside the repository is refused.
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
            validate_campaign_name(campaign)?;
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

/// Where Campaign state lives when `--state` and `--state-root` are omitted.
fn default_campaigns_root() -> Result<PathBuf, String> {
    Ok(xdg_state_root()?.join("af/review/campaigns"))
}

fn default_campaign_state(campaign: &str) -> Result<PathBuf, String> {
    campaign_state_beneath(&default_campaigns_root()?, campaign)
}

fn campaign_id(campaign: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"af/campaign-id@1\0");
    digest.update(campaign.as_bytes());
    let digest = digest.finalize();
    format!("c-{}", review_core::hex::encode(&digest))
}

fn campaign_state_beneath(root: &Path, campaign: &str) -> Result<PathBuf, String> {
    validate_campaign_name(campaign)?;
    let root = resolve_filesystem_path(root)?;
    let selected = resolve_filesystem_path(&root.join(campaign_id(campaign)))?;
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
        .join(&review_core::hex::encode(&identity)[..16]))
}

fn validate_campaign_name(campaign: &str) -> Result<(), String> {
    validate_campaign_component(campaign)?;
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

fn validate_campaign_component(campaign: &str) -> Result<(), String> {
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
            validate_campaign_name(campaign)?;
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
    let provider_admission = match (
        args.provider_admission_tokens,
        args.provider_admission_wall_ms,
    ) {
        (None, None) => None,
        (Some(tokens), Some(wall_ms))
            if tokens > 0
                && wall_ms > 0
                && tokens <= review_core::json::SAFE_INTEGER_MAX as u64
                && wall_ms <= review_core::json::SAFE_INTEGER_MAX as u64 =>
        {
            Some(review_graph::task::OperatorAttemptCost { tokens, wall_ms })
        }
        _ => usage_error(
            command,
            "Provider admission requires both positive, finite token and millisecond bounds",
        ),
    };
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
        provider_admission,
        json: args.json,
    }
}

fn age_label(unix_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let days = now.saturating_sub(unix_ms) / 86_400_000;
    match days {
        0 => "today".to_string(),
        1 => "1 day ago".to_string(),
        days => format!("{days} days ago"),
    }
}

#[derive(serde::Serialize)]
struct GcCandidateView {
    label: String,
    id: String,
    state_dir: String,
    state_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_activity_unix_ms: Option<u64>,
    age_days: u64,
    action: &'static str,
}

/// Reclaim Campaign state. Without `--apply` it only lists what would go; with it, whole
/// Campaign directories are removed — never CAS objects inside a live Campaign, never anything
/// the enumeration could not read (those are reported as problems and left alone).
fn print_gc(options: &GcOptions) -> Result<(), String> {
    let requested_root = match &options.state_root {
        Some(root) => root.clone(),
        None => default_campaigns_root()?,
    };
    let root = resolve_filesystem_path(&requested_root)?;
    let enumeration = enumerate_campaigns(&root, true)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let mut by_activity: Vec<&CampaignView> = enumeration.campaigns.iter().collect();
    by_activity
        .sort_by_key(|campaign| std::cmp::Reverse(campaign.last_activity_unix_ms.unwrap_or(0)));
    let kept: BTreeSet<&str> = by_activity
        .iter()
        .take(options.keep.unwrap_or(0))
        .map(|campaign| campaign.id.as_str())
        .collect();
    let mut candidates = Vec::new();
    let mut reclaimed = 0_u64;
    for campaign in &enumeration.campaigns {
        let age_days = now.saturating_sub(campaign.last_activity_unix_ms.unwrap_or(0)) / 86_400_000;
        let old_enough = options
            .older_than_days
            .is_none_or(|threshold| age_days >= threshold);
        let candidate = old_enough && !kept.contains(campaign.id.as_str());
        let action = if !candidate {
            "keep"
        } else if options.apply {
            let path = root.join(&campaign.state_dir);
            if std::fs::symlink_metadata(&path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(true)
            {
                return Err(format!(
                    "refusing to remove {}: not a plain directory",
                    path.display()
                ));
            }
            std::fs::remove_dir_all(&path)
                .map_err(|error| format!("removing {}: {error}", path.display()))?;
            reclaimed = reclaimed.saturating_add(campaign.state_bytes.unwrap_or(0));
            "removed"
        } else {
            "would remove"
        };
        candidates.push(GcCandidateView {
            label: campaign.label.clone(),
            id: campaign.id.clone(),
            state_dir: campaign.state_dir.clone(),
            state_bytes: campaign.state_bytes.unwrap_or(0),
            last_activity_unix_ms: campaign.last_activity_unix_ms,
            age_days,
            action,
        });
    }
    let reclaimable: u64 = candidates
        .iter()
        .filter(|candidate| candidate.action != "keep")
        .map(|candidate| candidate.state_bytes)
        .sum();
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "af/review-gc@1",
                "applied": options.apply,
                "older_than_days": options.older_than_days,
                "keep": options.keep,
                "campaigns": candidates,
                "reclaimable_bytes": reclaimable,
                "reclaimed_bytes": reclaimed,
                "problems": enumeration.problems,
            }))
            .map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    println!(
        "review gc ({}): {} campaigns, {} reclaimable{}",
        if options.apply {
            "applied"
        } else {
            "preview; add --apply to remove"
        },
        candidates.len(),
        human_bytes(reclaimable),
        if options.apply {
            format!(", {} reclaimed", human_bytes(reclaimed))
        } else {
            String::new()
        }
    );
    for candidate in &candidates {
        println!(
            "  {:<12} {} ({}): {}; {}",
            candidate.action,
            candidate.label.escape_debug(),
            candidate.state_dir,
            human_bytes(candidate.state_bytes),
            candidate
                .last_activity_unix_ms
                .map(age_label)
                .unwrap_or_else(|| "no store write recorded".to_string())
        );
    }
    for problem in &enumeration.problems {
        println!(
            "  skipped      {}: {}",
            problem.directory.escape_debug(),
            problem.reason.escape_debug()
        );
    }
    Ok(())
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
    default_campaigns_root()
        .map(|root| campaign_labels_beneath(&root))
        .unwrap_or_default()
}

fn campaign_labels_beneath(root: &Path) -> Vec<String> {
    enumerate_campaigns(root, false)
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

fn task_review_options(args: cli::RunArgs, plan_only: bool) -> task_execution::StartOptions {
    task_execution::StartOptions {
        file: args.task_file.expect("Task file path was checked"),
        bindings: args.bindings,
        source_bindings: None,
        repo: args.repo,
        state: args.state,
        authority: args.policy_rev.unwrap_or_else(|| "HEAD".into()),
        uncommitted: args.uncommitted,
        json: args.json,
        plan_only,
        timeout_secs: args.timeout_secs,
        optimization_history: None,
    }
}

fn review_command(namespace: cli::ReviewNamespace) -> Result<i32, String> {
    use cli::ReviewCommand as R;
    let command = match namespace.command {
        Some(command) => command,
        None => R::Run(namespace.run),
    };
    match command {
        R::Run(args) => {
            if args.task_file.is_some() {
                return task_execution::start_review(task_review_options(args, false));
            }
            let verdict = run(&run_options(args, "review run"))?;
            Ok(match verdict {
                RunVerdict::Pass => 0,
                RunVerdict::Fail(_) => 3,
                RunVerdict::Incomplete { .. } => 4,
            })
        }
        R::Plan(args) => {
            if args.task_file.is_some() {
                return task_execution::start_review(task_review_options(args, true));
            }
            print_plan(&run_options(args, "review plan")).map(|()| 0)
        }
        R::Render(args) => {
            if args.task_file.is_some() {
                return Err(
                    "Use af review plan --file, then af task explain for Task inspection".into(),
                );
            }
            print_render(&run_options(args, "review render")).map(|()| 0)
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
    let argv: Vec<String> = match std::env::args_os()
        .map(|argument| argument.into_string())
        .collect()
    {
        Ok(argv) => argv,
        Err(argument) => {
            eprintln!(
                "af: command-line argument {} is not valid UTF-8",
                argument.to_string_lossy()
            );
            std::process::exit(2);
        }
    };
    clap_complete::CompleteEnv::with_factory(cli::Af::command).complete();
    selfmgmt::maybe_dispatch(&argv);
    let parsed = cli::Af::parse_from(&argv);
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
                cli::ProviderCommand::Setup { id, kind, auth_dir } => providers::setup(
                    &id,
                    match kind {
                        cli::ProviderKindArg::Claude => "claude",
                        cli::ProviderKindArg::Codex => "codex",
                    },
                    auth_dir.as_deref(),
                )
                .map(|()| 0),
                cli::ProviderCommand::Add { id, kind, auth_dir } => providers::add(
                    &id,
                    match kind {
                        cli::ProviderKindArg::Claude => "claude",
                        cli::ProviderKindArg::Codex => "codex",
                    },
                    auth_dir.as_deref(),
                )
                .map(|()| 0),
                cli::ProviderCommand::Recover => providers::recover().map(|()| 0),
                cli::ProviderCommand::Doctor(args) => {
                    if args.task_file.is_some() {
                        Err(
                            "Task Provider admission does not use the Campaign doctor adapter"
                                .into(),
                        )
                    } else {
                        provider_doctor(&run_options(args, "provider doctor"))
                    }
                }
            },
        ),
        cli::Command::Onboard(args) => ("af onboard", onboard::run_cli(args).map(|()| 0)),
        cli::Command::Catalog {
            command:
                cli::CatalogCommand::Init {
                    profile,
                    developer_public_key,
                    repo,
                    destination,
                    json,
                },
        } => (
            "af catalog init",
            task_execution::starter::init(
                &repo,
                &destination,
                &profile,
                developer_public_key.as_deref(),
                json,
            )
            .map(|()| 0),
        ),
        cli::Command::Catalog {
            command:
                cli::CatalogCommand::Test {
                    source,
                    revision,
                    manifest,
                    fixtures,
                    pipeline,
                    worker,
                    json,
                },
        } => (
            "af catalog test",
            task_execution::catalog::test(
                &source,
                &revision,
                &manifest,
                fixtures.as_deref(),
                pipeline.as_deref(),
                worker.as_deref(),
                json,
            )
            .map(|()| 0),
        ),
        cli::Command::Catalog {
            command:
                cli::CatalogCommand::Sync {
                    source,
                    revision,
                    manifest,
                    repo,
                    destination,
                    json,
                },
        } => (
            "af catalog sync",
            task_execution::catalog::sync(&source, &revision, &manifest, &repo, &destination, json)
                .map(|()| 0),
        ),
        cli::Command::Task { command } => (
            "af task",
            match command {
                cli::TaskCommand::Start {
                    file,
                    execute,
                    bindings,
                    source_bindings,
                    repo,
                    state,
                    authority,
                    uncommitted,
                    timeout_secs,
                    json,
                } => task_execution::start(task_execution::StartOptions {
                    file,
                    bindings,
                    source_bindings,
                    repo,
                    state,
                    authority,
                    uncommitted,
                    json,
                    plan_only: !execute,
                    timeout_secs,
                    optimization_history: None,
                }),
                cli::TaskCommand::Plan {
                    file,
                    bindings,
                    source_bindings,
                    repo,
                    state,
                    authority,
                    uncommitted,
                    json,
                } => task_execution::start(task_execution::StartOptions {
                    file,
                    bindings,
                    source_bindings,
                    repo,
                    state,
                    authority,
                    uncommitted,
                    json,
                    plan_only: true,
                    timeout_secs: None,
                    optimization_history: None,
                }),
                cli::TaskCommand::DecisionPayload {
                    task_id,
                    developer,
                    decision,
                    reason,
                    output,
                    inspect,
                } => task_execution::developer::payload_file(
                    &task_id,
                    &developer,
                    if decision == "approved" {
                        review_core::task::plan::PlanDecisionKindV1::Approved
                    } else {
                        review_core::task::plan::PlanDecisionKindV1::Rejected
                    },
                    &reason,
                    &output,
                    &inspect,
                ),
                cli::TaskCommand::Approve {
                    task_id,
                    payload,
                    signature,
                    inspect,
                } => task_execution::developer::apply(
                    &task_id,
                    review_core::task::plan::PlanDecisionKindV1::Approved,
                    &payload,
                    &signature,
                    &inspect,
                ),
                cli::TaskCommand::Reject {
                    task_id,
                    payload,
                    signature,
                    inspect,
                } => task_execution::developer::apply(
                    &task_id,
                    review_core::task::plan::PlanDecisionKindV1::Rejected,
                    &payload,
                    &signature,
                    &inspect,
                ),
                cli::TaskCommand::Refresh {
                    task_id,
                    source_file,
                    source_bindings,
                    inspect,
                } => task_execution::refresh::refresh(
                    &task_id,
                    source_file.as_deref(),
                    source_bindings.as_deref(),
                    &inspect,
                ),
                cli::TaskCommand::Run {
                    task_id,
                    confirm_plan,
                    execute,
                    inspect,
                } => task_execution::run(
                    &task_id,
                    &inspect.repo,
                    inspect.state.as_deref(),
                    inspect.json,
                    confirm_plan.as_deref(),
                    execute,
                ),
                cli::TaskCommand::Output {
                    task_id,
                    port,
                    format,
                    output,
                    inspect,
                } => task_execution::domain::write_output(
                    &task_id,
                    &port,
                    &format,
                    &output,
                    &inspect.repo,
                    inspect.state.as_deref(),
                    inspect.json,
                ),
                cli::TaskCommand::Export {
                    task_id,
                    name,
                    destination,
                    inspect,
                } => task_execution::export::run(
                    &task_id,
                    &name,
                    &destination,
                    &inspect.repo,
                    inspect.state.as_deref(),
                    inspect.json,
                ),
                cli::TaskCommand::Explain {
                    task_id,
                    tree,
                    plan,
                    inspect,
                } => task_execution::explain(
                    &task_id,
                    &inspect.repo,
                    inspect.state.as_deref(),
                    inspect.json,
                    plan.as_deref(),
                    tree,
                ),
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
                cli::TaskCommand::ObserveAdoption {
                    task_id,
                    commit,
                    workload,
                    model,
                    environment,
                    evidence_task,
                    inspect,
                } => task::observe_adoption(
                    &task_id,
                    &commit,
                    &workload,
                    &model,
                    &environment,
                    evidence_task.as_deref(),
                    &inspect.repo,
                    inspect.state.as_ref(),
                    inspect.json,
                )
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
                cli::SelfCommand::Optimize {
                    since,
                    all_history,
                    strategy,
                    history_config,
                    execute,
                    experiment,
                    candidate,
                    repo,
                    state,
                    json,
                } => self_optimizer::run(self_optimizer::Options {
                    since,
                    all_history,
                    strategy,
                    history_config,
                    execute,
                    experiment,
                    candidate,
                    repo,
                    state,
                    json,
                }),
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
    println!(
        "provider admission  {} tokens, {} ms per capability (new capture; resume retains captured cost)",
        plan["provider_admission"]["tokens"], plan["provider_admission"]["wall_ms"]
    );
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
    let store = open_campaign_store_read_only(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
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
            "latest round Ledger: not produced because {}; showing the last gathered projection",
            evidence.absence_reason()
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
            print_indented("fix", &finding.fix);
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
    let task_accounting =
        report_tasks::read(&store, &cas, &campaign_run_id(&options.campaign), &events)?;
    let wall = task_accounting.wall_ms();
    let task_summary = report_tasks::summary(&task_accounting.tasks);
    if !task_summary.is_empty() {
        eprintln!("{task_summary}");
    }
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
            attached.report_id
        );
        let report = cas
            .get_json(&attached.report_id)
            .map_err(|e| format!("reading report {}: {e}", attached.report_id))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
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
    #[serde(skip)]
    task_accounting: Vec<report_tasks::TaskAccountingView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    findings: Option<FindingsSummaryView>,
    /// The state directory's name beneath the root, and — when asked for — what it holds.
    state_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_bytes: Option<u64>,
    /// Newest write to the event store, as Unix milliseconds; what `gc` ages by.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_activity_unix_ms: Option<u64>,
}

/// Bytes under a state directory, following no symlinks. A directory that cannot be read counts
/// as zero rather than failing the listing.
fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let Ok(metadata) = entry.metadata() else {
                return 0;
            };
            if metadata.is_dir() {
                directory_bytes(&entry.path())
            } else if metadata.is_file() {
                metadata.len()
            } else {
                0
            }
        })
        .sum()
}

fn newest_store_write_unix_ms(state: &Path) -> Option<u64> {
    ["events.sqlite", "events.sqlite-wal"]
        .iter()
        .filter_map(|name| std::fs::metadata(state.join(name)).ok())
        .filter_map(|metadata| metadata.modified().ok())
        .filter_map(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
        .max()
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn print_campaigns(options: &CampaignsOptions) -> Result<(), String> {
    let requested_root = match &options.state_root {
        Some(root) => root.clone(),
        None => default_campaigns_root()?,
    };
    let root = resolve_filesystem_path(&requested_root)?;
    let enumeration = enumerate_campaigns(&root, options.sizes)?;
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

fn enumerate_campaigns(root: &Path, with_sizes: bool) -> Result<CampaignEnumeration, String> {
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
        if let Err(reason) = validate_campaign_component(&label) {
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
                    "campaign {label:?} state directory must be its opaque ID `{id}` or its label"
                ),
            });
            continue;
        }
        if !seen.insert(id.clone()) {
            return Err(format!(
                "campaign {label:?} has state under both its opaque ID and its label beneath {}; remove the ambiguity before continuing",
                root.display()
            ));
        }
        let campaign = match read_campaign_view(
            &state,
            run_id,
            label.clone(),
            id.clone(),
            &store,
            with_sizes,
        ) {
            Ok(campaign) => campaign,
            Err(reason) => {
                problems.push(CampaignProblemView { directory, reason });
                continue;
            }
        };
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
    with_sizes: bool,
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
    let task_accounting = report_tasks::read(store, &cas, run_id, &events)?;
    let wall_ms = task_accounting.wall_ms();
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
        task_accounting: task_accounting.tasks,
        findings,
        state_dir: state
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        state_bytes: with_sizes.then(|| directory_bytes(state)),
        last_activity_unix_ms: newest_store_write_unix_ms(state),
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
        println!(
            "  state: {}{}{}",
            campaign.state_dir,
            campaign
                .state_bytes
                .map(|bytes| format!(", {}", human_bytes(bytes)))
                .unwrap_or_default(),
            campaign
                .last_activity_unix_ms
                .map(|at| format!("; last activity {}", age_label(at)))
                .unwrap_or_default()
        );
        if let Some(wall) = campaign.wall_ms {
            println!("  wall: {}", human_duration(wall));
        }
        for summary in report_tasks::summary(&campaign.task_accounting).lines() {
            println!("  {summary}");
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
                "    run {}: round {} epoch {}; {}; {}",
                round.run,
                optional_number(round.round),
                optional_number(round.epoch),
                round.verdict,
                round.tokens_label()
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    task_accounting: Vec<report_tasks::TaskAccountingView>,
    demands: Vec<review_core::DemandSetEntryV1>,
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
    task_chargeable_tokens_at_report: review_core::task::usage::DecimalU128,
    task_accounting: review_core::TaskReviewAccountingV1,
}

impl ReportRoundView {
    fn tokens_label(&self) -> String {
        format!(
            "Task cumulative charge at report {}",
            self.task_chargeable_tokens_at_report.get()
        )
    }

    fn tokens_cell(&self) -> String {
        format!(
            "{} (Task cumulative at report)",
            self.task_chargeable_tokens_at_report.get()
        )
    }
}

#[derive(serde::Serialize, Clone)]
struct AttemptWarmView {
    layers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rendered_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_tokens: Option<u64>,
    /// The node's Warm Workspace preparation for this Round, joined from `WorkspaceRebased@1`:
    /// it ran before the Attempt was reserved, so no Attempt wall clock includes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<WorkspacePreparationView>,
}

#[derive(serde::Serialize, Clone)]
struct WorkspacePreparationView {
    basis: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback: Option<String>,
    entries_touched: u64,
    preparation_ms: u64,
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
fn wall_span_ms<U>(rows: &[review_store::AttemptWall<U>]) -> Option<u64> {
    wall_spans_ms(rows.iter().map(|row| {
        (
            (row.round, row.epoch),
            (row.started_unix_ms, row.elapsed_ms),
        )
    }))
}

fn wall_spans_ms(rows: impl IntoIterator<Item = ((u32, u32), (u64, u64))>) -> Option<u64> {
    let mut spans: BTreeMap<(u32, u32), (u64, u64)> = BTreeMap::new();
    for (round, (started, elapsed)) in rows {
        let end = started.saturating_add(elapsed);
        spans
            .entry(round)
            .and_modify(|(start, finish)| {
                *start = (*start).min(started);
                *finish = (*finish).max(end);
            })
            .or_insert((started, end));
    }
    if spans.is_empty() {
        return None;
    }
    Some(spans.values().fold(0_u64, |sum, (start, finish)| {
        sum.saturating_add(finish.saturating_sub(*start))
    }))
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

/// Whether any pinned reviewer policy keeps a Warm Workspace (`warm = { workspace = "rebase" }`).
fn keeps_warm_workspace(loaded: &review_config::Loaded) -> bool {
    loaded
        .warm_policies()
        .values()
        .any(|policy| policy.workspace == review_config::WorkspaceSpec::Rebase)
}

/// Which warm layers an Attempt used and what its rendered input cost; empty for cold nodes.
fn attempt_warm_suffix(warm: Option<&AttemptWarmView>) -> String {
    let Some(warm) = warm else {
        return String::new();
    };
    let layers = if warm.layers.is_empty() {
        "none".to_string()
    } else {
        warm.layers.join(",")
    };
    let rendered = match (warm.rendered_bytes, warm.estimated_tokens) {
        (Some(bytes), Some(tokens)) => format!(" rendered {bytes} B (~{tokens} tokens)"),
        (Some(bytes), None) => format!(" rendered {bytes} B"),
        _ => String::new(),
    };
    let workspace = warm
        .workspace
        .as_ref()
        .map(|workspace| {
            let basis = match &workspace.fallback {
                Some(reason) => format!("{}:{reason}", workspace.basis),
                None => workspace.basis.clone(),
            };
            format!(
                "; workspace {basis} ({} touched, {})",
                workspace.entries_touched,
                human_duration(workspace.preparation_ms)
            )
        })
        .unwrap_or_default();
    format!(", warm[{layers}]{rendered}{workspace}")
}

fn print_report(options: &ReportOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let store = open_campaign_store_read_only(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let view = read_report_view(&store, &cas, &options.campaign)?;
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

fn read_report_view(
    store: &EventStore,
    cas: &Cas,
    campaign: &str,
) -> Result<ReviewReportView, String> {
    let run_id = campaign_run_id(campaign);
    let ledger = LedgerProjection::rebuild(store, cas, &run_id)
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
    let task_accounting = report_tasks::read(store, cas, &run_id, &events)?;
    let wall_ms = task_accounting.wall_ms();
    let findings = ledger.finding_views();
    Ok(ReviewReportView {
        schema: if task_accounting.tasks.is_empty() {
            "af/review-report@1"
        } else if task_accounting.has_wide_usage() {
            "af/review-report@4"
        } else {
            "af/review-report@3"
        },
        campaign: campaign.into(),
        runs_recorded: reports.len(),
        ledger_round: ledger.round,
        final_verdict: reports
            .last()
            .map(|event| run_report(event).map(|report| render_verdict_v3(report.verdict)))
            .transpose()?,
        rounds,
        task_accounting: task_accounting.tasks,
        demands: ledger.demand_views(),
        wall_ms,
        findings_summary: findings_summary(&findings),
        findings,
    })
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
            let report = run_report(event)?;
            Ok(ReportRoundView {
                run: index + 1,
                round: authority.map(|(round, _)| *round),
                epoch: authority.map(|(_, epoch)| *epoch),
                verdict: render_verdict_v3(report.verdict),
                task_chargeable_tokens_at_report: report.spent_tokens,
                task_accounting: report.task_accounting,
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
            "  run {} (round {} epoch {}): {}; {}",
            round.run,
            optional_number(round.round),
            optional_number(round.epoch),
            round.verdict,
            round.tokens_label()
        );
    }
    print!("{}", report_tasks::render(&report.task_accounting, false));
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
        println!("    fix: {}", one_line(&finding.fix));
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
            round.tokens_cell()
        );
    }
    println!();
    print!("{}", report_tasks::render(&report.task_accounting, true));
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
            println!("  - Fix: {}", markdown_line(&finding.fix));
            let evidence = finding
                .reports
                .iter()
                .map(|attached| {
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

fn run_report(event: &review_core::RunEvent) -> Result<RunReportPayloadV6, String> {
    if !event.event_type.is_run_report() {
        return Err(format!("{} is not a run report", event.event_type));
    }
    serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())
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
        other => {
            return Err(format!(
                "unsupported direct Resolution outcome `{other}` (use rejected or wontfix-tracked; a fix is proven with `af review attest-change` then `af review verify-fix`)"
            ));
        }
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

fn candidate_identity() -> Result<CandidateIdentity, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let bytes = std::fs::read(&executable).map_err(|error| error.to_string())?;
    Ok(CandidateIdentity {
        version: env!("CARGO_PKG_VERSION"),
        executable: executable.display().to_string(),
        binary_sha256: format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(bytes))
        ),
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

struct LatestRoundEvidence {
    ledger_production: &'static str,
}

impl LatestRoundEvidence {
    fn ledger_was_not_produced(&self) -> bool {
        self.ledger_production.starts_with("not_produced_")
    }

    fn absence_reason(&self) -> &'static str {
        match self.ledger_production {
            "not_produced_upstream_missing" => "required upstream output was missing",
            "not_produced_failed" => "the Ledger node failed",
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
        }));
    }

    let report = events.iter().rev().find(|event| {
        event.event_type.is_run_report()
            && event.causation_id.as_deref() == Some(round_event.event_id.as_str())
    });
    let Some(report) = report else {
        return Ok(None);
    };
    let outcomes = run_report(report)?.outcomes;
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome.node == ledger_node_id)
        .ok_or("Run Report has no outcome for the pinned Ledger node")?;
    let ledger_production = match outcome.outcome {
        review_core::RunNodeOutcomeV2::Suppressed {
            reason: review_core::RunSuppressionReasonV2::UpstreamMissing,
        } => "not_produced_upstream_missing",
        review_core::RunNodeOutcomeV2::Failed { .. } => "not_produced_failed",
        review_core::RunNodeOutcomeV2::Completed { .. } => {
            return Err("Ledger completed without a NodeOutputReceipt".into());
        }
    };
    Ok(Some(LatestRoundEvidence { ledger_production }))
}

fn packaged_runner(command: &review_core::Command) -> String {
    Path::new(&command.program)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn provider_doctor(options: &Options) -> Result<i32, String> {
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
    let campaign = campaign_run_id(options.campaign.as_deref().unwrap_or("local"));
    review_task::require_common_campaign(&cas, &store, &campaign)?;
    review_task::doctor(options, &cas, &mut store, &repo, &campaign)
}

fn run(options: &Options) -> Result<RunVerdict, String> {
    let state = options.resolved_state_dir()?;
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|error| error.to_string())?;
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(&options.repo, &git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let campaign = campaign_run_id(options.campaign.as_deref().unwrap_or("local"));
    review_task::require_common_campaign(&cas, &store, &campaign)?;
    review_task::run(options, &cas, &mut store, &repo, &campaign)
}

#[cfg(test)]
mod option_tests {
    use super::{
        CampaignMode, Options, campaign_id, campaign_labels_beneath, campaign_state_beneath,
        default_campaigns_root, enumerate_campaigns, latest_round_evidence, validate_campaign_name,
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
            "version = 2\n[subject]\nkind = \"whole-tree\"\n[[nodes]]\nid = \"{ledger_node}\"\nkind = \"ledger\"\noutputs = [{{ name = \"findings\", type = \"review.kernel/FindingSet@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"any\" }}]\n"
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
                path: ".af/af.lock".into(),
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
            check_timeout_seconds: 3600,
            git_timeout_seconds: 300,
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
        for valid in ["heavy", "af-tui", "round_4", "v2.1-audit"] {
            assert!(validate_campaign_name(valid).is_ok());
        }
    }

    #[test]
    fn campaign_state_uses_an_opaque_contained_id() {
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

        // A directory named by the label is never a fallback for the default state root.
        write_campaign_opening(&root.join("heavy"), "heavy");
        assert_eq!(campaign_state_beneath(&root, "heavy").unwrap(), encoded);

        // An explicit `--state ROOT/<label>` may name the same Campaign, so enumeration refuses
        // one Campaign held under both names.
        write_campaign_opening(&encoded, "heavy");
        let Err(error) = enumerate_campaigns(&root, false) else {
            panic!("ambiguous Campaign state was enumerated");
        };
        assert!(
            error.contains("both its opaque ID and its label"),
            "{error}"
        );
        assert!(
            campaign_state_beneath(&root, &id)
                .unwrap_err()
                .contains("reserved opaque Campaign ID shape")
        );
    }

    #[test]
    fn completion_lists_the_labels_under_the_campaigns_root() {
        if let Ok(root) = default_campaigns_root() {
            assert!(root.ends_with("af/review/campaigns"), "{}", root.display());
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("af/review/campaigns");
        write_campaign_opening(&root.join(campaign_id("heavy")), "heavy");
        write_campaign_opening(&root.join("loop"), "loop");
        assert_eq!(campaign_labels_beneath(&root), ["heavy", "loop"]);
        // The state root above it holds no Campaign directly.
        assert!(campaign_labels_beneath(temp.path()).is_empty());
    }

    #[test]
    fn campaign_enumeration_does_not_create_a_missing_cas() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("campaigns");
        let state = root.join("missing-cas");
        write_campaign_opening(&state, "missing-cas");
        std::fs::remove_dir_all(state.join("cas")).unwrap();

        let enumeration = enumerate_campaigns(&root, false).unwrap();
        assert!(enumeration.campaigns.is_empty());
        assert_eq!(enumeration.problems.len(), 1);
        assert!(!state.join("cas").exists());
    }

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
            uncommitted: false,
            restart_round: false,
            mode: CampaignMode::Light,
            timeout: None,
            git_timeout: None,
            provider_bindings: std::collections::BTreeMap::new(),
            provider_admission: None,
            json: false,
            node: None,
        };
        let error = options.resolved_state_dir().unwrap_err();
        assert!(error.contains("state must live under XDG state"));
    }

    /// `af review run` JSON and the `af review ledger` notice classify the latest Round's Ledger
    /// from its output receipt, or from the Round's Run Report when the Ledger produced none.
    #[test]
    fn latest_round_ledger_production_is_read_from_its_receipt_or_run_report() {
        let temp = tempfile::tempdir().unwrap();
        let cas = review_store::Cas::open(temp.path()).unwrap();
        let (manifest_id, findings_id, demands_id) = campaign_manifest(&cas, "reduce");
        let report = |outcome: review_core::RunNodeOutcomeV2| {
            serde_json::to_value(review_core::RunReportPayloadV6 {
                outcomes: vec![review_core::RunNodeReportV2 {
                    node: "reduce".into(),
                    outcome,
                }],
                blocked_gates: Vec::new(),
                verdict: review_core::RunVerdictV3::Incomplete {
                    missing_nodes: Vec::new(),
                },
                spent_tokens: 37_u128.into(),
                task_accounting: review_core::TaskReviewAccountingV1 {
                    task_id: "review".into(),
                    task_revision_id: findings_id.clone(),
                    plan_id: findings_id.clone(),
                    task_report_id: findings_id.clone(),
                    through_sequence: 1,
                },
                execution: review_core::RunReportExecutionV6::Unbound {},
            })
            .unwrap()
        };
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
                    prior_finding_set_id: findings_id.clone(),
                    prior_demand_set_id: demands_id,
                })
                .unwrap(),
            ),
            event(
                1,
                review_core::EventType::RunReportV6,
                Some("event-0"),
                None,
                None,
                report(review_core::RunNodeOutcomeV2::Suppressed {
                    reason: review_core::RunSuppressionReasonV2::UpstreamMissing,
                }),
            ),
        ];
        let evidence = latest_round_evidence(&events, &cas).unwrap().unwrap();
        assert_eq!(evidence.ledger_production, "not_produced_upstream_missing");
        assert!(evidence.ledger_was_not_produced());

        let mut gathered = events.clone();
        gathered.insert(
            1,
            event(
                1,
                review_core::EventType::NodeOutputReceiptV1,
                Some("event-0"),
                Some("reduce"),
                None,
                serde_json::json!({"node": "reduce", "outputs": []}),
            ),
        );
        let evidence = latest_round_evidence(&gathered, &cas).unwrap().unwrap();
        assert_eq!(evidence.ledger_production, "produced_with_findings");

        let mut failed = events;
        failed.last_mut().unwrap().payload = report(review_core::RunNodeOutcomeV2::Failed {
            error: "invalid Ledger output".into(),
        });
        let evidence = latest_round_evidence(&failed, &cas).unwrap().unwrap();
        assert_eq!(evidence.ledger_production, "not_produced_failed");
    }
}
