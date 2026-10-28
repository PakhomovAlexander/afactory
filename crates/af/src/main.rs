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

mod af_paths;
mod agent_setup;
mod authority;
mod caches;
mod cli;
mod config;
mod doctor;
mod error_document;
mod human;
mod interrupt;
mod model_check;
mod model_failures;
mod onboard;
mod power;
mod project;
/// Project Briefs (ADR-0160), beside the Surveyor they render from.
#[path = "projects/brief.rs"]
mod project_brief;
mod projects;
mod providers;
mod report_tasks;
mod review_task;
mod self_optimizer;
mod selfmgmt;
mod storage;
mod store_release;
mod task;
mod task_execution;
mod telemetry;
mod topics;
mod tui;

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
        let repository = std::fs::canonicalize(&self.repo).map_err(|error| {
            format!(
                "opening repository {}: {error}",
                terminal_text(&self.repo.display().to_string())
            )
        })?;
        if state.starts_with(&repository) {
            return Err(format!(
                "state {} is inside the repository; af state must live under XDG state or an explicit external --state directory",
                state.display()
            ));
        }
        if self.state.is_some() {
            // An explicit directory becomes af-owned: one holding anything else is refused
            // before anything is written or registered (ADR-0144).
            storage::owned::admit(&state)?;
            storage::registry::record(&state, storage::registry::StoreKind::Review);
        }
        Ok(state)
    }
}

/// The state root, its existing prefix resolved (`config::canonical_root`), so every Store af
/// keeps beneath it is named and measured without a link in its path.
fn xdg_state_root() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("XDG_STATE_HOME") {
        let configured = PathBuf::from(configured);
        if !configured.is_absolute() {
            return Err("XDG_STATE_HOME must be absolute".to_string());
        }
        return Ok(config::canonical_root(&configured));
    }
    let user_home =
        std::env::var_os("HOME").ok_or("HOME is not set and XDG_STATE_HOME is absent")?;
    let user_home = PathBuf::from(user_home);
    if !user_home.is_absolute() {
        return Err("HOME must be absolute".to_string());
    }
    Ok(config::canonical_root(&user_home.join(".local/state")))
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
    let repository = std::fs::canonicalize(repository).map_err(|error| {
        format!(
            "opening repository {}: {error}",
            terminal_text(&repository.display().to_string())
        )
    })?;
    let identity = Sha256::digest(repository.as_os_str().as_encoded_bytes());
    Ok(xdg_state_root()?
        .join("af/review/local")
        .join(&review_core::hex::encode(&identity)[..16]))
}

/// The longest Campaign label, in bytes: listings show it whole.
const MAX_CAMPAIGN_LABEL_BYTES: usize = 128;

/// Whether `character` is a Unicode format character (general category Cf, the bidi controls
/// among them) or a line or paragraph separator (Zl, Zp): invisible in a listing, or able to
/// reorder or break the text around it.
fn is_invisible_or_reordering(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

fn validate_campaign_name(campaign: &str) -> Result<(), String> {
    if campaign.len() > MAX_CAMPAIGN_LABEL_BYTES {
        return Err(format!(
            "campaign name is {} bytes; a campaign name is at most {MAX_CAMPAIGN_LABEL_BYTES} bytes",
            campaign.len()
        ));
    }
    if let Some(character) = campaign.chars().find(|c| is_invisible_or_reordering(*c)) {
        return Err(format!(
            "campaign name {campaign:?} holds U+{:04X}, an invisible format, bidi control or \
             separator character that could disguise it in a listing",
            u32::from(character)
        ));
    }
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
    let mut resolved = std::fs::canonicalize(&existing).map_err(|error| {
        format!(
            "opening {}: {error}",
            terminal_text(&existing.display().to_string())
        )
    })?;
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
            storage::owned::admit(&state)?;
            storage::registry::record(&state, storage::registry::StoreKind::Review);
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
    eprintln!(
        "error: {}\n  try: af {} --help",
        terminal_lines(message),
        terminal_text(command)
    );
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
    /// Why a candidate is kept although old enough: it is in use (ADR-0144).
    #[serde(skip_serializing_if = "Option::is_none")]
    in_use: Option<String>,
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
    if options.state_root.is_some() {
        storage::registry::record(&root, storage::registry::StoreKind::ReviewRoot);
    }
    let enumeration = enumerate_campaigns(&root, true)?;
    // Each campaign directory's identity as enumerated: only that directory is ever removed.
    let measured: BTreeMap<String, Option<review_sandbox::Identity>> = enumeration
        .campaigns
        .iter()
        .map(|campaign| {
            let identity = review_sandbox::Identity::of(&root.join(&campaign.state_dir)).ok();
            (campaign.state_dir.clone(), identity)
        })
        .collect();
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
        // A campaign a running `af review run` holds, or whose Task holds a live writer lease,
        // is never removed (ADR-0144).
        // With --apply it is held exclusively from this look through the removal, so no run
        // starts on it in between; a preview only looks, and writes nothing.
        let held = (candidate && options.apply)
            .then(|| storage::hold_campaign_for_removal(&root.join(&campaign.state_dir)));
        let live = match &held {
            Some(Err(why)) => Some(why.clone()),
            Some(Ok(_)) => None,
            None => candidate
                .then(|| storage::campaign_in_use(&root.join(&campaign.state_dir)))
                .flatten(),
        };
        let action = if !candidate {
            "keep"
        } else if live.is_some() {
            "in use"
        } else if options.apply {
            let path = root.join(&campaign.state_dir);
            let Some(identity) = measured.get(&campaign.state_dir).copied().flatten() else {
                return Err(format!(
                    "refusing to remove {}: its identity could not be measured",
                    path.display()
                ));
            };
            // Through descriptors from the root, no link followed, and only while it is still
            // the directory enumerated (ADR-0144).
            if options.state_root.is_some() {
                // Below an explicit root, whole only when it is proven af's; otherwise af's
                // entries alone, and what was left is said.
                let removed = storage::owned::remove(&root, &path, identity)
                    .map_err(|error| format!("removing {}: {error}", path.display()))?;
                if let Some(line) = storage::owned::left_line(&path, &removed.left) {
                    eprintln!("af review gc: {}", terminal_text(&line));
                }
                reclaimed = reclaimed.saturating_add(removed.bytes);
            } else {
                review_sandbox::remove_beneath(&root, &path, Some(identity))
                    .map_err(|error| format!("removing {}: {error}", path.display()))?;
                reclaimed = reclaimed.saturating_add(campaign.state_bytes.unwrap_or(0));
            }
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
            in_use: live,
        });
    }
    let reclaimable: u64 = candidates
        .iter()
        .filter(|candidate| !matches!(candidate.action, "keep" | "in use"))
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
            "  {:<12} {} ({}): {}; {}{}",
            candidate.action,
            terminal_text(&candidate.label),
            terminal_text(&candidate.state_dir),
            human_bytes(candidate.state_bytes),
            candidate
                .last_activity_unix_ms
                .map(age_label)
                .unwrap_or_else(|| "no store write recorded".to_string()),
            candidate
                .in_use
                .as_deref()
                .map(|why| format!("; {}", terminal_text(why)))
                .unwrap_or_default()
        );
    }
    for problem in &enumeration.problems {
        println!(
            "  skipped      {}: {}",
            terminal_text(&problem.directory),
            terminal_text(&problem.reason)
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
        fit: false,
        replace: false,
    }
}

fn review_command(namespace: cli::ReviewNamespace) -> Result<i32, String> {
    use cli::ReviewCommand as R;
    let command = match namespace.command {
        Some(command) => command,
        None => {
            let words: Vec<String> = std::env::args().skip(1).collect();
            if words.len() != 1 || words[0] != "review" {
                return review_command(cli::ReviewNamespace {
                    command: Some(R::Run(namespace.run.clone())),
                    run: namespace.run,
                });
            }
            let options = run_options(namespace.run, "review plan");
            if let Err(message) = review_selector_refusal(&options, "plan") {
                eprintln!("{}", terminal_lines(&message));
            }
            use clap::CommandFactory as _;
            cli::Af::command()
                .find_subcommand_mut("review")
                .expect("review exists")
                .print_long_help()
                .map_err(|error| error.to_string())?;
            return Ok(2);
        }
    };
    match command {
        R::Run(args) => {
            if args.task_file.is_some() {
                return task_execution::start_review(task_review_options(args, false));
            }
            let options = run_options(args, "review run");
            let verdict = run(&options)?;
            Ok(match verdict {
                RunVerdict::Pass => 0,
                RunVerdict::Fail(_) => 3,
                RunVerdict::Incomplete { .. } => 4,
            })
        }
        R::Plan(args) => {
            // A plan reads each bound Provider's catalog token-free and names every Model
            // binding's verdict (ADR-0148).
            model_check::enable_plan_time();
            if args.task_file.is_some() {
                return task_execution::start_review(task_review_options(args, true));
            }
            let options = run_options(args, "review plan");
            review_selector_refusal(&options, "plan")?;
            print_plan(&options).map(|()| 0)
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
            state: selector.state.clone(),
            campaign: selector.campaign.clone(),
            long,
        })
        .map_err(|error| explain_campaign(&selector, error))
        .map(|()| 0),
        R::Show { selector, key } => show(&ShowOptions {
            state: selector.state.clone(),
            campaign: selector.campaign.clone(),
            key,
        })
        .map_err(|error| explain_campaign(&selector, error))
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
            state: selector.state.clone(),
            campaign: selector.campaign.clone(),
            format: match format {
                cli::ReportFormatArg::Md => ReportFormat::Markdown,
                cli::ReportFormatArg::Text => ReportFormat::Text,
                cli::ReportFormatArg::Json => ReportFormat::Json,
            },
        })
        .map_err(|error| explain_campaign(&selector, error))
        .map(|()| 0),
        R::Tui => {
            eprintln!(
                "error: `af review tui` is not a command; the browser is `af` alone, at a \
                 terminal\n  try: af"
            );
            Ok(2)
        }
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
            json,
            sizes,
        } => print_campaigns(&CampaignsOptions {
            state_root,
            sizes,
            // `--json` is `--format json`; clap refuses the two together.
            format: match (json, format) {
                (true, _) | (false, cli::ListFormatArg::Json) => CampaignsFormat::Json,
                (false, cli::ListFormatArg::Text) => CampaignsFormat::Text,
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

/// Build a copyable review command from the invocation, keeping every explicit flag.
fn review_next_command(subcommand: &str, extra: &[String]) -> String {
    let mut words: Vec<String> = std::env::args()
        .skip(1)
        .filter(|word| word != "--json")
        .collect();
    if words.first().is_some_and(|word| word == "review") {
        if words
            .get(1)
            .is_some_and(|word| word == "plan" || word == "run")
        {
            words[1] = subcommand.into();
        } else {
            words.insert(1, subcommand.into());
        }
    }
    let mut command = vec!["af".to_string()];
    command.extend(words);
    command.extend(extra.iter().cloned());
    shell_words::join(command)
}

/// Local Git metadata and isolated captures. Never consult the candidate's `.af/` to choose
/// authority.
fn review_selector_refusal(options: &Options, subcommand: &str) -> Result<(), String> {
    if options.policy_rev.is_some()
        || subcommand == "run"
            && options.campaign.is_some()
            && options
                .resolved_state_dir()
                .ok()
                .is_some_and(|state| state.join("events.sqlite").is_file())
    {
        return Ok(());
    }
    let git = |args: &[&str]| -> Option<String> {
        let output = review_process::reap::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&options.repo)
                .args([
                    "-c",
                    "core.fsmonitor=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args),
        )
        .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    if git(&["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return Err(error_document::typed(
            format!(
                "{} is not a Git repository; choose a repository with a commit",
                terminal_text(&options.repo.display().to_string())
            ),
            error_document::Detail::new(error_document::Cause::NotRepository)
                .with("repo", options.repo.display().to_string()),
        ));
    }
    let current = git(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let remote = git(&[
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .and_then(|name| name.strip_prefix("origin/").map(str::to_owned));
    let configured = git(&["config", "--get", "init.defaultBranch"]);
    let local = ["main", "master"]
        .into_iter()
        .find(|name| git(&["show-ref", "--verify", &format!("refs/heads/{name}")]).is_some());
    let chosen = [remote, configured, local.map(str::to_owned)]
        .into_iter()
        .flatten()
        .find_map(|name| {
            git(&[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{name}^{{commit}}"),
            ])
            .or_else(|| {
                git(&[
                    "rev-parse",
                    "--verify",
                    &format!("refs/remotes/origin/{name}^{{commit}}"),
                ])
            })
            .map(|commit| (name, commit))
        });
    let mut candidate_details = serde_json::Map::new();
    let next = chosen.as_ref().and_then(|(branch, commit)| {
        let on_default = current.as_deref() == Some(branch.as_str());
        let scratch = tempfile::tempdir().ok()?;
        let cas = Cas::open(scratch.path().join("cas")).ok()?;
        let git_home = scratch.path().join("git-home");
        std::fs::create_dir_all(&git_home).ok()?;
        let repo = Repo::open(&options.repo, &git_home);
        let policy = if on_default {
            "HEAD".to_owned()
        } else {
            commit.clone()
        };
        let mut flags = vec!["--policy-rev".into(), policy.clone()];
        if options.base.is_none() {
            flags.extend(["--base".to_owned(), policy.clone()]);
        }
        let mut uncommitted = options.uncommitted;
        let mut candidate = options.candidate.clone();
        if on_default {
            // Capture reads worktree bytes directly. Git sees only isolated plumbing, so a
            // candidate-selected clean filter cannot run before review has a sandbox.
            let capture = review_source_git::Capture::new(&repo, &cas);
            let dirty = capture
                .committed("HEAD")
                .and_then(|head| {
                    capture
                        .dirty()
                        .map(|worktree| head.content_digest != worktree.content_digest)
                })
                .unwrap_or(false);
            if !dirty && candidate.is_none() && !uncommitted {
                return None;
            }
            if candidate.is_none() && !uncommitted {
                flags.push("--uncommitted".into());
                uncommitted = true;
            }
        } else if candidate.is_none() && !uncommitted {
            flags.extend(["--candidate".to_owned(), "HEAD".to_owned()]);
            candidate = Some("HEAD".to_owned());
        }
        let mut selected = options.clone();
        selected.campaign = None;
        selected.restart_round = false;
        selected.policy_rev = Some(policy.clone());
        selected.base = options.base.clone().or(Some(policy));
        selected.candidate = candidate;
        selected.uncommitted = uncommitted;
        let resolved = authority::resolve_plan(&selected, &cas, &repo).ok()?;
        let registered = providers::registered().ok()?.0;
        let mut unique = true;
        for (node, kind) in authority::model_workers(&resolved.loaded) {
            if selected.provider_bindings.contains_key(&node) {
                continue;
            }
            let ids: Vec<String> = registered
                .iter()
                .filter(|(id, registered_kind)| {
                    *registered_kind == kind && providers::availability::check_id(id).is_ok()
                })
                .map(|(id, _)| id.clone())
                .collect();
            if ids.len() == 1 {
                flags.extend(["--provider".to_owned(), format!("{node}={}", ids[0])]);
            } else {
                unique = false;
            }
            candidate_details.insert(node, serde_json::json!(ids));
        }
        unique
            .then(|| review_next_command(subcommand, &flags))
            .filter(|command| command.len() <= 4095 && !command.chars().any(char::is_control))
    });
    let message = if let Some(command) = &next {
        format!(
            "af review {subcommand} needs --policy-rev REV and an explicit Subject; run:\n{command}"
        )
    } else {
        let candidates = if candidate_details.is_empty() {
            String::new()
        } else {
            format!(
                "; Provider candidates: {}",
                serde_json::Value::Object(candidate_details.clone())
            )
        };
        format!(
            "af review {subcommand} needs --policy-rev REV; the trusted policy revision must be chosen when the default branch cannot be determined, and Provider bindings must be chosen when ambiguous{candidates}"
        )
    };
    let mut detail = error_document::Detail::new(error_document::Cause::ReviewSelectorsMissing)
        .with("repo", options.repo.display().to_string())
        .with("selector", "--policy-rev");
    if !candidate_details.is_empty() {
        detail = detail.with("candidates", serde_json::Value::Object(candidate_details));
    }
    if let Some(command) = next {
        detail = detail.next(command);
    }
    Err(error_document::typed(message, detail))
}

fn review_binding_refusal(
    options: &Options,
    workers: &[(String, String)],
    subcommand: &str,
) -> Result<(), String> {
    let registered = providers::registered()
        .map(|(ids, _)| ids)
        .unwrap_or_default();
    let mut missing = Vec::new();
    let mut candidates = serde_json::Map::new();
    let mut flags = Vec::new();
    let mut unique = true;
    for (node, kind) in workers {
        if options.provider_bindings.contains_key(node) {
            continue;
        }
        missing.push(node.clone());
        let ids: Vec<String> = registered
            .iter()
            .filter(|(id, registered_kind)| {
                *registered_kind == kind && providers::availability::check_id(id).is_ok()
            })
            .map(|(id, _)| id.clone())
            .collect();
        if ids.len() == 1 {
            flags.extend(["--provider".to_owned(), format!("{node}={}", ids[0])]);
        } else {
            unique = false;
        }
        candidates.insert(node.clone(), serde_json::json!(ids));
    }
    if missing.is_empty() {
        return Ok(());
    }
    let next = unique
        .then(|| review_next_command(subcommand, &flags))
        .filter(|command| command.len() <= 4095 && !command.chars().any(char::is_control));
    let message = match &next {
        Some(command) => format!(
            "model Workers {} need explicit Provider bindings; run:\n{command}",
            missing.join(", ")
        ),
        None => format!(
            "model Workers {} need explicit Provider bindings; choose one enabled Provider per Worker from {:?}, then pass --provider NODE=ID",
            missing.join(", "),
            candidates
        ),
    };
    let mut detail = error_document::Detail::new(error_document::Cause::ReviewBindingMissing)
        .with("missing", serde_json::json!(missing))
        .with("candidates", serde_json::Value::Object(candidates));
    if let Some(command) = next {
        detail = detail.next(command);
    }
    Err(error_document::typed(message, detail))
}

fn help_command(words: &[String], snippet: bool) -> Result<i32, String> {
    use clap::CommandFactory as _;
    if snippet {
        if words == ["agents"] {
            print!("{}", topics::agent_snippet());
            return Ok(0);
        }
        return Err("--snippet belongs to `af help agents`; run af help agents --snippet".into());
    }
    let mut root = cli::Af::command();
    if words.is_empty() {
        root.print_long_help().map_err(|error| error.to_string())?;
        println!("\n{}", topics::listing());
        return Ok(0);
    }
    if words.first().is_some_and(|word| word == "errors") && words.len() == 2 {
        println!("{}", error_document::explain(Some(&words[1]))?);
        return Ok(0);
    }
    if words.len() == 1
        && let Some((topic, about, text)) = topics::find(&words[0])
    {
        let body = topics::page(topic, text);
        println!("af help {topic} — {about}\n\n{body}");
        return Ok(0);
    }
    let mut current = &mut root;
    for word in words {
        match current.find_subcommand_mut(word) {
            Some(sub) => current = sub,
            None => {
                let topics: Vec<&str> = topics::TOPICS.iter().map(|(name, _, _)| *name).collect();
                eprintln!(
                    "error: Topic or command {} not found in af help; topics: {}\n  try: af --help",
                    terminal_text(&words.join(" ")),
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
    use review_core::perf_trace::{self, Phase};
    // Inert outside a `perf-trace` diagnostic build (ADR-0174): startup composition, then the
    // dispatched command, exported when the command returns through here.
    let startup = perf_trace::span(Phase::CliStartup);
    let argv: Vec<String> = match std::env::args_os()
        .map(|argument| argument.into_string())
        .collect()
    {
        Ok(argv) => argv,
        Err(argument) => {
            eprintln!(
                "af: command-line argument {} is not valid UTF-8",
                terminal_text(&argument.to_string_lossy())
            );
            std::process::exit(2);
        }
    };
    // The calibration loop of a laboratory slowdown trial runs inside this process, before
    // anything else (ADR-0181); a build without `perf-calibration` refuses its control.
    if let Err(error) = review_core::perf_calibration::configure_from_env() {
        eprintln!("af: {error}");
        std::process::exit(2);
    }
    if review_core::perf_calibration::inject().is_some()
        && let Some(line) = review_core::perf_calibration::report_line()
    {
        eprintln!("{line}");
    }
    if let Some(code) = providers::auth_handoff::supervise_early(&argv) {
        std::process::exit(code);
    }
    clap_complete::CompleteEnv::with_factory(cli::Af::command).complete();
    // `af catalog digest` reads one directory and writes nothing (ADR-0157), so it runs and
    // exits before every self-management hook: it never dispatches to or installs a pinned
    // release, schedules an update check or touches af's own state.
    if let Ok(cli::Af {
        version: false,
        command:
            Some(cli::Command::Catalog {
                command: cli::CatalogCommand::Digest { package },
            }),
        ..
    }) = cli::Af::try_parse_from(&argv)
    {
        if let Err(error) = task_execution::catalog::digest(&package) {
            eprintln!("af catalog digest: {error}");
            std::process::exit(1);
        }
        return;
    }
    selfmgmt::maybe_dispatch(&argv);
    // `parse_from`, with the trace exported before help, usage and errors exit.
    let parsed = match cli::Af::try_parse_from(&argv) {
        Ok(parsed) => parsed,
        Err(error) => {
            drop(startup);
            perf_trace::finish();
            if argv.len() == 2
                && argv[1] == "review"
                && let Ok(cli::Af {
                    command: Some(cli::Command::Review(namespace)),
                    ..
                }) = cli::Af::try_parse_from(["af", "review", "plan"])
                && let Some(cli::ReviewCommand::Plan(args)) = namespace.command
                && let Err(suggestion) =
                    review_selector_refusal(&run_options(args, "review plan"), "plan")
            {
                eprintln!("{}", terminal_lines(&suggestion));
            }
            // Under --json a usage error is the one `af/error@2` document `af help json`
            // promises, beside clap's own message on stderr and with its exit code.
            if let Some(document) = error_document::usage(&argv, &error) {
                println!("{document}");
                let _ = std::io::stdout().flush();
            }
            error.exit()
        }
    };
    if parsed.version {
        if let Err(error) = selfmgmt::print_version(parsed.json) {
            eprintln!("af: {}", terminal_lines(&error.to_string()));
            std::process::exit(1);
        }
        drop(startup);
        perf_trace::finish();
        return;
    }
    let Some(command) = parsed.command else {
        if !tui::wanted() {
            // Not a terminal: the usage error a bare `af` has always been, help on stderr, after
            // the first-run hints.
            eprintln!("{}\n", cli::FIRST_RUN_HINTS);
            let refused = cli::Af::command()
                .arg_required_else_help(true)
                .try_get_matches_from(["af"]);
            match refused {
                Err(error) => error.exit(),
                Ok(_) => std::process::exit(2),
            }
        }
        let code = tui::launch(parsed.repo.as_deref()).unwrap_or_else(|error| {
            eprintln!("af: {}", terminal_lines(&error.to_string()));
            1
        });
        selfmgmt::after_command(&argv);
        std::process::exit(code);
    };
    // Still the only thread: every thread started from here inherits the blocked signals. A run
    // `af task run --detach` started outlives the terminal that started it (ADR-0211).
    if std::env::var_os(task_execution::runs::DETACHED_ENV).is_some()
        && matches!(
            &command,
            cli::Command::Task {
                command: cli::TaskCommand::Run { .. }
            }
        )
        && let Err(error) = interrupt::ignore_hangup()
    {
        eprintln!("af: {}", terminal_lines(&error.to_string()));
        std::process::exit(1);
    }
    if needs_interrupt_handling(&command)
        && let Err(error) = interrupt::install()
    {
        eprintln!("af: {}", terminal_lines(&error.to_string()));
        std::process::exit(1);
    }
    // The Storage Budget first: it names the sandbox root (`[storage] sandbox_root`) the stale
    // sandbox sweep below looks in.
    storage::set_output(runs_task_work(&command), verbose_sweeps(&command));
    // A Mac does not idle-sleep while Task work runs; `af task run --verbose` says so
    // (ADR-0209). Held until af exits.
    let _power = runs_task_work(&command)
        .then(|| {
            power::hold(matches!(
                &command,
                cli::Command::Task {
                    command: cli::TaskCommand::Run { verbose: true, .. }
                }
            ))
        })
        .flatten();
    if runs_task_work(&command)
        && let Err(error) = storage::install()
    {
        // The free-disk floor and the budget step before a new warm key (ADR-0144). Without
        // them no check or Worker may start, so work is refused rather than run unguarded.
        eprintln!(
            "af: the storage budget cannot be set up, so no Task or review work starts: {}; \
             see `af config show --origin` for [storage] and the XDG directories",
            terminal_lines(&error.to_string())
        );
        std::process::exit(1);
    }
    let sandbox_sweep = runs_task_work(&command)
        .then(sweep_stale_sandboxes_in_background)
        .flatten();
    let collects_after = collects_after_run(&command);
    let task_work = runs_task_work(&command);
    // A command that runs a kernel run records it (ADR-0153); every other command opens no
    // telemetry directory.
    let mut recording =
        recorded_run(&command).map(|kind| telemetry::begin(kind, telemetry::command_chain(&argv)));
    // Registered best effort (ADR-0158): `af task plan|start` before `--workspace` resolves,
    // every other command once it ran.
    let registers = projects::lazy_target(&command);
    if let Some(lazy) = registers.as_ref().filter(|lazy| lazy.first) {
        projects::register_lazily(lazy);
    }
    let delivered = projects::delivered_target(&command);
    // `af onboard --apply` records the first Survey once it registered (ADR-0159).
    let surveyed = match &command {
        cli::Command::Onboard(args) if args.apply => Some((args.repo.clone(), args.json)),
        _ => None,
    };
    drop(startup);
    let dispatched = perf_trace::span(Phase::CliCommand);
    let (prefix, outcome): (&str, Result<i32, String>) = match command {
        cli::Command::Review(namespace) => ("af review", review_command(namespace)),
        cli::Command::Provider { command } => ("af provider", provider_command(command)),
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
            command: cli::CatalogCommand::Digest { package },
        } => (
            "af catalog digest",
            task_execution::catalog::digest(&package).map(|()| 0),
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
        cli::Command::Task { command } => ("af task", task_command(command)),
        cli::Command::Project {
            command:
                cli::ProjectCli::Brief {
                    variant,
                    survey,
                    json,
                },
        } => ("af project", project_brief::command(variant, survey, json)),
        cli::Command::Project {
            command: cli::ProjectCli::Core(command),
        } => {
            let sections = match &command {
                cli::ProjectCommand::Survey { json: false } => cli::survey_sections(&argv),
                _ => Vec::new(),
            };
            ("af project", projects::survey::run(command, &sections))
        }
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
                cli::SelfCommand::Status { json, refresh } => {
                    selfmgmt::status(json, refresh).map(|()| 0)
                }
                cli::SelfCommand::Update { check, version, rc } => {
                    selfmgmt::update(check, version, rc).map(|()| 0)
                }
                cli::SelfCommand::Rollback => selfmgmt::rollback().map(|()| 0),
                cli::SelfCommand::Install { version } => {
                    selfmgmt::install_command(&version).map(|()| 0)
                }
                cli::SelfCommand::Remove { version } => selfmgmt::remove(&version).map(|()| 0),
                cli::SelfCommand::Prune => selfmgmt::prune().map(|()| 0),
                cli::SelfCommand::SetupShell {
                    shell,
                    write,
                    remove,
                } => selfmgmt::setup_shell(shell, write, remove).map(|()| 0),
                cli::SelfCommand::Uninstall { purge } => selfmgmt::uninstall(purge).map(|()| 0),
                cli::SelfCommand::Man { out_dir } => selfmgmt::man(&out_dir).map(|()| 0),
                cli::SelfCommand::RefreshCheck => selfmgmt::refresh_check().map(|()| 0),
            },
        ),
        cli::Command::Storage {
            json,
            verbose,
            command,
        } => (
            "af storage",
            match command {
                None => storage::show(json, verbose),
                Some(cli::StorageCommand::Prune {
                    apply,
                    json: prune_json,
                }) => storage::prune(apply, json || prune_json, verbose),
            },
        ),
        cli::Command::Telemetry { command } => match command {
            cli::TelemetryCommand::Status { json, dir } => {
                ("af telemetry status", telemetry::status(json, dir))
            }
            cli::TelemetryCommand::Reindex { json, dir } => {
                ("af telemetry reindex", telemetry::reindex(json, dir))
            }
            cli::TelemetryCommand::Runs {
                since,
                kind,
                json,
                dir,
            } => ("af telemetry runs", telemetry::runs(json, dir, since, kind)),
            cli::TelemetryCommand::Show {
                run,
                store,
                attempt,
                span,
                depth,
                json,
                dir,
            } => (
                "af telemetry show",
                telemetry::show(json, dir, run, store, attempt, span, depth),
            ),
            cli::TelemetryCommand::Turns {
                run,
                store,
                attempt,
                json,
                dir,
            } => (
                "af telemetry turns",
                telemetry::turns(json, dir, run, store, attempt),
            ),
            cli::TelemetryCommand::Tools {
                run,
                store,
                attempt,
                json,
                dir,
            } => (
                "af telemetry tools",
                telemetry::tools(json, dir, run, store, attempt),
            ),
            cli::TelemetryCommand::Context {
                run,
                attempt,
                store,
                raw,
                json,
                dir,
            } => (
                "af telemetry context",
                telemetry::context(json, raw, dir, run, attempt, store),
            ),
            cli::TelemetryCommand::Reasoning {
                run,
                attempt,
                store,
                json,
                dir,
            } => (
                "af telemetry reasoning",
                telemetry::reasoning(json, dir, run, attempt, store),
            ),
            cli::TelemetryCommand::Search {
                text,
                since,
                run,
                store,
                json,
                dir,
            } => (
                "af telemetry search",
                telemetry::search(json, dir, text, since, run, store),
            ),
            cli::TelemetryCommand::Config {
                run,
                store,
                execution,
                json,
                dir,
            } => (
                "af telemetry config",
                telemetry::config(json, dir, run, store, execution),
            ),
            cli::TelemetryCommand::Tail {
                run,
                store,
                json,
                dir,
            } => ("af telemetry tail", telemetry::tail(json, dir, run, store)),
            cli::TelemetryCommand::Ledger {
                run,
                store,
                json,
                dir,
            } => (
                "af telemetry ledger",
                telemetry::ledger(json, dir, run, store),
            ),
            cli::TelemetryCommand::Gc { apply, json } => {
                ("af telemetry gc", telemetry::gc(json, apply))
            }
        },
        cli::Command::Completions { shell } => (
            "af completions",
            selfmgmt::completion_script(shell).map(|script| {
                print!("{script}");
                0
            }),
        ),
        cli::Command::Doctor(args) => ("af doctor", doctor::run(&args.repo, args.json)),
        cli::Command::Help { words, snippet } => ("af help", help_command(&words, snippet)),
        cli::Command::Setup { command } => match command {
            cli::SetupCommand::Agents { repo, write } => {
                ("af setup agents", agent_setup::run(&repo, write))
            }
        },
    };
    if let Some(signal) = interrupt::received() {
        task_execution::print_last_lines();
        if let Some(recording) = recording.take() {
            recording.finish(
                interrupt::exit_code(signal),
                review_telemetry::record::SealExit::Signal,
            );
        }
        exit_interrupted(&argv, prefix, signal, outcome.err());
    }
    if let Some(lazy) = registers.as_ref().filter(|lazy| !lazy.first) {
        projects::register_lazily(lazy);
    }
    if let Some((repo, json)) = &surveyed {
        projects::survey::after_onboard(repo, *json);
    }
    if let (Some((worktree, task_id)), Ok(0)) = (&delivered, &outcome) {
        projects::register_delivered(worktree, task_id);
    }
    if collects_after {
        // Whatever the run's outcome; a failure here is a warning, never the exit status.
        storage::after_run();
    }
    if task_work {
        // One line for every sweep the command ran, the floor's and a new warm key's included.
        storage::flush_summary();
    }
    let code = match outcome {
        Ok(code) => code,
        Err(error) => {
            // A classified refusal adds its cause, next command and facts (`af help json`).
            let detail = error_document::take(&error);
            if argv.iter().skip(1).any(|word| word == "--json") {
                let document = error_document::document(prefix, &error, 1, detail.as_ref());
                println!("{document}");
            }
            eprintln!("{prefix}: {}", terminal_lines(&error));
            1
        }
    };
    if let Some(recording) = recording.take() {
        recording.finish(
            code,
            if code == 0 {
                review_telemetry::record::SealExit::Ok
            } else {
                review_telemetry::record::SealExit::Error
            },
        );
    }
    if let Some(sweep) = sandbox_sweep {
        // The command's own work is done; a sweep still removing a large leftover finishes
        // before the process ends, so a stale directory never outlives two Task-running commands.
        if let Ok(Some(line)) = sweep.join() {
            eprintln!("{line}");
        }
    }
    drop(dispatched);
    perf_trace::finish();
    selfmgmt::after_command(&argv);
    // The run log's last line, the Task's state and next command, after everything the
    // command's housekeeping printed: a script reads it as stderr's last line.
    task_execution::print_last_lines();
    if code != 0 {
        std::process::exit(code);
    }
}

/// `af task`: `--workspace` resolved first, then the subcommand.
fn task_command(command: cli::TaskCommand) -> Result<i32, String> {
    let command = projects::resolve_task_workspace(command)?;
    match command {
        cli::TaskCommand::Start {
            file,
            execute,
            verbose: _,
            bindings,
            source_bindings,
            repo,
            state,
            authority,
            uncommitted,
            timeout_secs,
            json,
            workspace: _,
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
            fit: false,
            replace: false,
        }),
        cli::TaskCommand::Plan {
            file,
            bindings,
            source_bindings,
            repo,
            state,
            authority,
            uncommitted,
            fit,
            replace,
            json,
            workspace: _,
        } => {
            // A plan reads each bound Provider's catalog token-free and names every
            // Model binding's verdict (ADR-0148).
            model_check::enable_plan_time();
            task_execution::start(task_execution::StartOptions {
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
                fit,
                replace,
            })
        }
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
            detach: true,
            inspect,
            ..
        } => task_execution::runs::detach(&task_id, &inspect),
        // `--from` conflicts with `--detach` at parse time (cli.rs), so the detach arm never
        // sees a `--from`; the continue arm precedes the foreground arm, which matches `..`.
        cli::TaskCommand::Run {
            task_id,
            from: Some(from),
            round,
            goal,
            bindings,
            execute,
            inspect,
            ..
        } => task_execution::run_from(task_execution::ContinueOptions {
            task_id,
            from,
            round,
            goal,
            bindings,
            repo: inspect.repo,
            state: inspect.state,
            json: inspect.json,
            execute,
        }),
        cli::TaskCommand::Run {
            task_id,
            confirm_plan,
            execute,
            retry_gate,
            verbose: _,
            detach: false,
            quiet,
            inspect,
            ..
        } => task_execution::run(
            &task_id,
            &inspect.repo,
            inspect.state.as_deref(),
            inspect.json,
            confirm_plan.as_deref(),
            execute,
            retry_gate,
            quiet,
        ),
        cli::TaskCommand::Wait {
            task_id,
            timeout,
            inspect,
        } => task_execution::runs::wait(&task_id, timeout, &inspect),
        cli::TaskCommand::Status { task_id, inspect } => {
            task_execution::runs::status(&task_id, &inspect)
        }
        cli::TaskCommand::Cancel { task_id, inspect } => {
            task_execution::runs::cancel(&task_id, &inspect)
        }
        cli::TaskCommand::Findings {
            task_id,
            round,
            inspect,
        } => task_execution::findings::print(&task_id, round, &inspect),
        cli::TaskCommand::Materialize {
            task_id,
            snapshot,
            into,
            allow_unverified,
            force,
            inspect,
        } => task_execution::candidate::materialize(
            &task_id,
            snapshot.as_deref(),
            &into,
            allow_unverified,
            force,
            &inspect,
        ),
        cli::TaskCommand::Diff {
            task_id,
            snapshot,
            base,
            stat,
            name_only,
            inspect,
        } => task_execution::candidate::diff(
            &task_id,
            snapshot.as_deref(),
            base.as_deref(),
            stat,
            name_only,
            &inspect,
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
            output.as_deref(),
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
            workspace: _,
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
        cli::TaskCommand::List { sizes, inspect } => {
            task::inspect_from_cli(None, inspect.repo, inspect.state, inspect.json)
                .and_then(|options| task::list(options, sizes))
                .map(|()| 0)
        }
        cli::TaskCommand::Gc {
            older_than,
            keep,
            apply,
            inspect,
        } => task_execution::collection::gc(
            older_than,
            keep,
            apply,
            &inspect.repo,
            inspect.state.as_deref(),
            inspect.json,
        ),
        cli::TaskCommand::Show {
            task_id,
            checks,
            inspect,
        } => task::inspect_from_cli(Some(task_id), inspect.repo, inspect.state, inspect.json)
            .and_then(|options| task::show(options, checks))
            .map(|()| 0),
        cli::TaskCommand::Report {
            task_ids,
            pr_body,
            template,
            base,
            inspect,
        } => task::inspect_from_cli(None, inspect.repo, inspect.state, inspect.json)
            .and_then(|options| {
                let body = pr_body.then_some(task::PrBody {
                    template: template.as_deref(),
                    base: base.as_deref(),
                });
                task::report(options, &task_ids, body)
            })
            .map(|()| 0),
    }
}

/// The commands that run Task Workers through the shared Task host take over SIGINT and SIGTERM
/// so an interrupt stops those Workers before af exits (ADR-0129); `af self optimize --execute`
/// runs an Optimization Task's Workers the same way (issue #158).
fn runs_task_work(command: &cli::Command) -> bool {
    match command {
        cli::Command::Review(namespace) => {
            matches!(namespace.command, None | Some(cli::ReviewCommand::Run(_)))
        }
        cli::Command::Provider { command } => matches!(command, cli::ProviderCommand::Doctor(_)),
        // `af task run --detach` only starts the run that does the work, in the background.
        cli::Command::Task { command } => matches!(
            command,
            cli::TaskCommand::Start { execute: true, .. }
                | cli::TaskCommand::Run { detach: false, .. }
        ),
        cli::Command::SelfCmd { command } => {
            matches!(command, cli::SelfCommand::Optimize { execute: true, .. })
        }
        _ => false,
    }
}

/// Whether a Task command asked for every line of its storage sweeps (`--verbose`) instead of
/// the one summary line.
fn verbose_sweeps(command: &cli::Command) -> bool {
    matches!(
        command,
        cli::Command::Task {
            command: cli::TaskCommand::Start { verbose: true, .. }
                | cli::TaskCommand::Run { verbose: true, .. }
        }
    )
}

/// The kind of kernel run a command runs, when it runs one: it records Telemetry (design §6).
/// Every other command records nothing and opens no telemetry directory.
fn recorded_run(command: &cli::Command) -> Option<review_pipeline::telemetry::RunKind> {
    use review_pipeline::telemetry::RunKind;
    match command {
        cli::Command::Review(namespace) => {
            matches!(namespace.command, None | Some(cli::ReviewCommand::Run(_)))
                .then_some(RunKind::Review)
        }
        cli::Command::Provider {
            command: cli::ProviderCommand::Doctor(_),
        } => Some(RunKind::Review),
        cli::Command::Task {
            command:
                cli::TaskCommand::Start { execute: true, .. }
                | cli::TaskCommand::Run { detach: false, .. },
        } => Some(RunKind::Task),
        cli::Command::SelfCmd {
            command: cli::SelfCommand::Optimize { execute: true, .. },
        } => Some(RunKind::Optimize),
        _ => None,
    }
}

/// `af task run`, `af task start --execute` and `af review run` end with the Storage Budget's
/// sweep: the budget always, age-based collection too when `[storage] auto_gc` is on (ADR-0144).
fn collects_after_run(command: &cli::Command) -> bool {
    match command {
        cli::Command::Review(namespace) => {
            matches!(namespace.command, None | Some(cli::ReviewCommand::Run(_)))
        }
        cli::Command::Task { command } => matches!(
            command,
            cli::TaskCommand::Start { execute: true, .. }
                | cli::TaskCommand::Run { detach: false, .. }
        ),
        _ => false,
    }
}

/// A private login owns native processes but never runs Task work or its maintenance sweep.
fn needs_interrupt_handling(command: &cli::Command) -> bool {
    runs_task_work(command)
        || matches!(
            command,
            cli::Command::Provider {
                command: cli::ProviderCommand::Auth {
                    command: cli::ProviderAuthCommand::Begin { .. }
                }
            }
        )
}

/// An interrupted command ends by its signal once its Workers have stopped, so a shell reports
/// the conventional status (130 for SIGINT, 143 for SIGTERM), and says how to resume. A failure
/// other than the interrupt itself is still reported first; `--json` carries the same status.
fn exit_interrupted(
    argv: &[String],
    prefix: &str,
    signal: nix::sys::signal::Signal,
    error: Option<String>,
) -> ! {
    let code = interrupt::exit_code(signal);
    // The same next command `af task show --json` names: `af task run ID` only while the
    // interrupted node has a recovery left (ADR-0209). Either carries the Store selectors this
    // command was given.
    let resume = match (interrupt::noted_task(), interrupt::noted_next()) {
        (Some(id), Some(next)) => format!(
            "a node of Task {id} used up its recovery allowance, so `af task run {id}` cannot \
             continue it; start it again with `{next}` (the Task file it was started from)"
        ),
        (Some(id), None) => format!(
            "resume it with `{}`, which dispatches the interrupted node again without using \
             one of its Attempts",
            task_execution::with_selectors(&format!("af task run {id}"))
        ),
        (None, _) => "run the same command again to resume it".into(),
    };
    let message = if argv.get(1).is_some_and(|s| s == "provider")
        && argv.get(2).is_some_and(|s| s == "auth")
    {
        format!(
            "interrupted by {signal}; native login stopped. Inspect the non-secret recovery status; a new login requires a fresh private host permission"
        )
    } else {
        format!(
            "interrupted by {signal}; its Worker processes were stopped and the Task was not finished. To continue, {resume}"
        )
    };
    if let Some(error) = error {
        if error != interrupt::INTERRUPTED {
            eprintln!("{prefix}: {}", terminal_lines(&error));
        }
        if argv.iter().skip(1).any(|word| word == "--json") {
            let document = serde_json::json!({
                "schema": "af/error@2",
                "command": prefix,
                "error": message,
                "exit_code": code,
            });
            println!("{document}");
        }
    }
    eprintln!("{prefix}: {}", terminal_lines(&message));
    let _ = std::io::stdout().flush();
    interrupt::exit(signal);
}

/// Every `af provider` command returns its own documented exit code.
///
/// The classified Provider outcomes — human action required, missing CLI, registry conflict,
/// authentication failure, unavailable optional usage — are results rather than crashes, so they
/// print themselves and come back as `Ok(code)`. Only an unclassified failure takes the shared
/// `Err` path and exits 1.
fn provider_command(command: cli::ProviderCommand) -> Result<i32, String> {
    match command {
        cli::ProviderCommand::Detect {
            dirs,
            ids,
            kind,
            all,
            apply,
            json,
            identity,
        } => providers::detect::run(providers::detect::DetectOptions {
            dirs,
            ids,
            kind: kind.map(|kind| provider_kind(kind).to_string()),
            all,
            apply,
            json,
            identity,
        }),
        cli::ProviderCommand::Status {
            json,
            usage,
            verbose,
        } => {
            let usage = if usage {
                providers::UsageProbe::Probe
            } else {
                providers::UsageProbe::Skip
            };
            providers::print_status(json, usage, verbose)
        }
        cli::ProviderCommand::Setup {
            id,
            kind,
            auth_dir,
            login,
            json,
        } => {
            let kind = provider_kind(kind);
            providers::setup(&id, kind, auth_dir.as_deref(), login, json)
        }
        cli::ProviderCommand::Models { ids, json, resolve } => {
            providers::models::print_models(&ids, json, resolve.as_deref())
        }
        cli::ProviderCommand::Add {
            id,
            kind,
            auth_dir,
            unverified,
        } => {
            let kind = provider_kind(kind);
            providers::add(&id, kind, auth_dir.as_deref(), unverified)
        }
        cli::ProviderCommand::Remove { ids } => providers::remove(&ids),
        cli::ProviderCommand::Disable { id, duration } => {
            providers::availability::disable(&id, &duration)
        }
        cli::ProviderCommand::Enable { id } => providers::availability::enable(&id),
        cli::ProviderCommand::Auth { command } => match command {
            cli::ProviderAuthCommand::Begin {
                id,
                kind,
                auth_dir,
                host_read_fd,
                host_write_fd,
                timeout_secs,
            } => providers::auth_handoff::begin(
                &id,
                provider_kind(kind),
                &auth_dir,
                host_read_fd,
                host_write_fd,
                timeout_secs,
            ),
            cli::ProviderAuthCommand::Status { id, kind, auth_dir } => {
                providers::auth_handoff::status(&id, provider_kind(kind), &auth_dir)
            }
            cli::ProviderAuthCommand::Cancel {
                id,
                kind,
                auth_dir,
                recovery_id,
            } => providers::auth_handoff::cancel(&id, provider_kind(kind), &auth_dir, &recovery_id),
        },
        cli::ProviderCommand::Recover => providers::recover().map(|()| 0),
        cli::ProviderCommand::Doctor(args) => {
            provider_doctor(&run_options(args.into(), "provider doctor"))
        }
    }
}

fn provider_kind(kind: cli::ProviderKindArg) -> &'static str {
    match kind {
        cli::ProviderKindArg::Claude => "claude",
        cli::ProviderKindArg::Codex => "codex",
    }
}

fn print_render(options: &Options) -> Result<(), String> {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|error| error.to_string())?;
    let repository = std::fs::canonicalize(&options.repo).map_err(|error| {
        format!(
            "opening repository {}: {error}",
            terminal_text(&options.repo.display().to_string())
        )
    })?;
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
    eprintln!(
        "node      {} ({})",
        terminal_text(&view.node),
        terminal_text(&view.runner)
    );
    if let Some(package) = &view.package {
        eprintln!(
            "package   {} {} {}",
            terminal_text(package["name"].as_str().unwrap_or("?")),
            terminal_text(package["version"].as_str().unwrap_or("?")),
            terminal_text(package["digest"].as_str().unwrap_or("?"))
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
            terminal_text(&entry.name),
            entry.rendered_bytes,
            terminal_text(&entry.required_by)
        );
    }
    for item in &view.not_rendered {
        eprintln!("omitted   {}", terminal_text(item));
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
    let repository = std::fs::canonicalize(&options.repo).map_err(|error| {
        format!(
            "opening repository {}: {error}",
            terminal_text(&options.repo.display().to_string())
        )
    })?;
    let git_home = scratch.path().join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(repository, git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let (mut plan, models) = authority::plan(options, &cas, &repo)?;
    let workers: Vec<(String, String)> = plan["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|provider| provider["required"] == true)
        .map(|provider| {
            (
                provider["node"].as_str().unwrap_or("?").to_owned(),
                provider["runner"].as_str().unwrap_or("?").to_owned(),
            )
        })
        .collect();
    review_binding_refusal(options, &workers, "plan")?;
    let empty = plan["subject"]["empty"].as_bool() == Some(true);
    let unfit = plan["pipeline"]["inputs_fit"].as_bool() == Some(false);
    let missing = missing_bindings(&plan);
    let next = plan_next_step(empty, unfit, &missing);
    if options.json {
        // The model lines go to stderr (ADR-0148); the next command the text prints is the
        // document's `next_action`, null where the text names no command.
        for line in &models {
            eprintln!("{}", terminal_text(line));
        }
        plan["next_action"] = serde_json::json!(next.command);
        println!(
            "{}",
            serde_json::to_string(&plan).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    let selectors = &plan["selectors"];
    let resolved = &plan["resolved"];
    plan_line!("review plan (token-free; no Campaign state)");
    if let Some(timeout) = options.timeout {
        plan_line!(
            "timeout  --timeout-secs {}: per reviewer Attempt",
            timeout.as_secs()
        );
    }
    plan_line!(
        "policy   {} => {}",
        terminal_text(selectors["policy_rev"].as_str().unwrap_or("?")),
        terminal_text(resolved["policy_snapshot_id"].as_str().unwrap_or("?"))
    );
    if let Some(base) = selectors["base"].as_str() {
        plan_line!(
            "base     {} => {}",
            terminal_text(base),
            terminal_text(resolved["base_snapshot_id"].as_str().unwrap_or("?"))
        );
    }
    plan_line!(
        "candidate {} => {}",
        terminal_text(selectors["candidate"].as_str().unwrap_or("?")),
        terminal_text(resolved["candidate_snapshot_id"].as_str().unwrap_or("?"))
    );
    for provider in plan["providers"].as_array().into_iter().flatten() {
        if provider["required"] == true {
            plan_line!(
                "binding  {} => {}",
                terminal_text(provider["node"].as_str().unwrap_or("?")),
                terminal_text(provider["binding"].as_str().unwrap_or("?"))
            );
        }
    }
    plan_line!(
        "subject  {} ({} paths, {} patch{})",
        terminal_text(plan["subject"]["kind"].as_str().unwrap_or("?")),
        plan["subject"]["changed_paths"]
            .as_array()
            .map_or(0, Vec::len),
        human::bytes(plan["subject"]["patch_bytes"].as_u64().unwrap_or(0)),
        if empty {
            "; EMPTY — run will refuse"
        } else {
            ""
        }
    );
    if empty {
        // What is empty, and the selectors that would fill it.
        plan_line!(
            "  empty  the diff from base {} to candidate {} changes no file; change files and \
             review them with --uncommitted, or name other revisions with --base REV and \
             --candidate REV",
            terminal_text(selectors["base"].as_str().unwrap_or("?")),
            terminal_text(selectors["candidate"].as_str().unwrap_or("?")),
        );
    }
    for path in plan["subject"]["changed_paths"]
        .as_array()
        .into_iter()
        .flatten()
    {
        plan_line!("  change  {}", terminal_text(path.as_str().unwrap_or("?")));
    }
    if let Some(route) = plan["route"].as_object() {
        plan_line!(
            "route    {} => {}{}{}",
            terminal_text(
                route
                    .get("policy")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
            ),
            terminal_text(
                route
                    .get("pipeline_path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
            ),
            route
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(|name| format!(" ({})", terminal_text(name)))
                .unwrap_or_default(),
            route
                .get("replaced")
                .and_then(serde_json::Value::as_str)
                .map(|replaced| format!(
                    "; replaced {}: its Worker input exceeded the cap",
                    terminal_text(replaced)
                ))
                .unwrap_or_default()
        );
    }
    plan_line!("topology");
    for node in plan["pipeline"]["topology"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        plan_line!(
            "  node {} [{}]",
            terminal_text(node["id"].as_str().unwrap_or("?")),
            terminal_text(node["kind"].as_str().unwrap_or("?"))
        );
    }
    for edge in plan["pipeline"]["topology"]["edges"]
        .as_array()
        .into_iter()
        .flatten()
    {
        plan_line!(
            "  {}.{} -> {}.{}",
            terminal_text(edge["from"]["node"].as_str().unwrap_or("?")),
            terminal_text(edge["from"]["port"].as_str().unwrap_or("?")),
            terminal_text(edge["to"]["node"].as_str().unwrap_or("?")),
            terminal_text(edge["to"]["port"].as_str().unwrap_or("?"))
        );
    }
    for gate in plan["pipeline"]["gates"].as_array().into_iter().flatten() {
        plan_line!(
            "gate     {} ({})",
            terminal_text(gate["name"].as_str().unwrap_or("?")),
            if gate["required"].as_bool() == Some(true) {
                "required"
            } else {
                "optional"
            }
        );
    }
    if let Some(budgets) = plan["pipeline"]["budgets"].as_object() {
        plan_line!(
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
                    terminal_text(reservation["node"].as_str().unwrap_or("?")),
                    reservation["tokens"].as_u64().unwrap_or(0),
                    terminal_text(reservation["source"].as_str().unwrap_or("?"))
                )
            })
            .collect::<Vec<_>>();
        plan_line!(
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
            plan_line!(
                "input    {} {} (about {} tokens){}",
                terminal_text(reservation["node"].as_str().unwrap_or("?")),
                human::bytes(bytes),
                reservation["input_tokens"].as_u64().unwrap_or(0),
                if reservation["fits"].as_bool() == Some(false) {
                    " — EXCEEDS its Attempt cap; run will refuse"
                } else {
                    ""
                }
            );
        }
    }
    plan_line!(
        "provider admission  {} tokens, {} per capability (new capture; resume retains captured cost)",
        plan["provider_admission"]["tokens"],
        human::duration(plan["provider_admission"]["wall_ms"].as_u64().unwrap_or(0))
    );
    // A missing binding names its fix: a registered Provider of the Worker's kind, or how to
    // register one when this machine has none.
    for provider in plan["providers"].as_array().into_iter().flatten() {
        let node = provider["node"].as_str().unwrap_or("?");
        let kind = provider["runner"].as_str().unwrap_or("?");
        match provider["binding"].as_str() {
            Some(binding) => plan_line!(
                "provider {} -> {}",
                terminal_text(node),
                terminal_text(binding)
            ),
            None if provider["required"] == true => {
                let fix = match missing
                    .iter()
                    .find(|(missing, _)| missing == node)
                    .and_then(|(_, id)| id.as_deref())
                {
                    Some(id) => format!("--provider {node}={id}"),
                    None => format!(
                        "no {kind} Provider is registered: `af provider status` lists what this \
                         machine has, and `af provider setup {kind}-main --kind {kind} --login` \
                         registers one"
                    ),
                };
                plan_line!(
                    "provider {} -> MISSING; fix: {}",
                    terminal_text(node),
                    terminal_text(&fix)
                );
            }
            None => plan_line!("provider {} -> MISSING", terminal_text(node)),
        }
    }
    for line in &models {
        plan_line!("{}", terminal_text(line));
    }
    let bound = plan["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|provider| provider["required"] == true && provider["binding"].is_string());
    if bound {
        // The plan read each bound Provider's catalog, token-free (ADR-0148).
        plan_line!(
            "effects  no state, Gates, Provider admission, Workers, or token spend; each bound Provider's model catalog was read token-free"
        );
    } else {
        plan_line!("effects  no state, Gates, Provider calls, Workers, or token spend");
    }
    // What to do, then the one command, whole: continued between its arguments when it is
    // wider than the terminal, so it still runs as copied.
    let width = human::width();
    let command = next.command.as_deref().map(terminal_text);
    match (next.prose.as_deref().map(terminal_text), command) {
        (Some(prose), Some(command)) => {
            println!("{}", human::wrap(&format!("next:    {prose}"), width, 9));
            println!(
                "{}",
                human::fit_command(&format!("         {command}"), width)
            );
        }
        (Some(prose), None) => println!("{}", human::wrap(&format!("next:    {prose}"), width, 9)),
        (None, Some(command)) => {
            println!(
                "{}",
                human::fit_command(&format!("next:    {command}"), width)
            );
        }
        (None, None) => println!("next:    none"),
    }
    Ok(())
}

/// One line of `af review plan`'s text, held to the terminal's width (`af help json` keeps the
/// document whole): prose wraps at spaces under the column its label's values start at, and a
/// command between backticks stays on one line.
macro_rules! plan_line {
    ($($argument:tt)*) => {
        println!("{}", wrap_plan_line(&format!($($argument)*)))
    };
}
use plan_line;

/// [`plan_line!`]'s wrapping: continuation lines start where the line's first value does
/// (`policy   HEAD => sha256:...` continues under `HEAD`), else two columns past its indent.
fn wrap_plan_line(line: &str) -> String {
    let width = human::width();
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    let column = rest
        .find("  ")
        .map(|at| lead + at + (rest[at..].len() - rest[at..].trim_start().len()))
        .filter(|column| *column <= width / 3)
        .unwrap_or(lead + 2);
    human::wrap(line, width, column)
}

/// The model Workers of `plan` without a Provider binding, each with a registered Provider of
/// its kind when this machine has one.
fn missing_bindings(plan: &serde_json::Value) -> Vec<(String, Option<String>)> {
    let registered = providers::registered()
        .map(|(ids, _)| ids)
        .unwrap_or_default();
    plan["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|provider| provider["binding"].is_null() && provider["required"] == true)
        .map(|provider| {
            let kind = provider["runner"].as_str().unwrap_or("?");
            let candidate = registered
                .iter()
                .find(|(_, registered_kind)| *registered_kind == kind)
                .map(|(id, _)| id.clone());
            (
                provider["node"].as_str().unwrap_or("?").to_owned(),
                candidate,
            )
        })
        .collect()
}

/// What `af review plan` says to do next: what to do, when that is more than one command, and
/// the one command to run, when there is one. The command is the document's `next_action`.
struct NextStep {
    prose: Option<String>,
    command: Option<String>,
}

/// What `af review plan` tells a person to do next: the exact `af review run` command when the
/// plan is what run admits, else the first thing that keeps it from running.
fn plan_next_step(empty: bool, unfit: bool, missing: &[(String, Option<String>)]) -> NextStep {
    let words: Vec<String> = std::env::args().skip(1).collect();
    let rerun = |subcommand: &str, extra: &[String]| {
        let mut command = vec!["af".to_string()];
        let mut replaced = false;
        for word in &words {
            if !replaced && word == "plan" {
                command.push(subcommand.to_string());
                replaced = true;
            } else if word != "--json" {
                command.push(word.clone());
            }
        }
        command.extend(extra.iter().cloned());
        shell_words::join(command)
    };
    if empty {
        return NextStep {
            prose: Some(
                "change files first; the diff is empty and `af review run` refuses it. Then:"
                    .into(),
            ),
            command: Some(rerun("plan", &[])),
        };
    }
    if unfit {
        return NextStep {
            prose: Some(
                "narrow the Subject (a smaller --base REV to --candidate REV range) or raise the \
                 Worker's budget.attempt, then plan again"
                    .into(),
            ),
            command: None,
        };
    }
    if !missing.is_empty() {
        if missing.iter().all(|(_, id)| id.is_some()) {
            let flags: Vec<String> = missing
                .iter()
                .flat_map(|(node, id)| {
                    [
                        "--provider".to_string(),
                        format!("{node}={}", id.as_deref().unwrap_or_default()),
                    ]
                })
                .collect();
            return NextStep {
                prose: None,
                command: Some(rerun("plan", &flags)),
            };
        }
        return NextStep {
            prose: Some(
                "register a Provider for each model Worker (`af provider setup ID --kind KIND \
                 --login`), then bind each with --provider NODE=ID"
                    .into(),
            ),
            command: None,
        };
    }
    NextStep {
        prose: None,
        command: Some(rerun("run", &[])),
    }
}

/// One progress line of `af review run`: findings, nodes and reasons a reviewer or the
/// repository authored included, so it is shown through [`terminal_text`].
fn run_progress(options: &Options, arguments: fmt::Arguments<'_>) {
    let line = terminal_text(&arguments.to_string());
    if options.json {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// The not-found refusal of a Campaign, in the shape every not-found refusal takes:
/// `<Thing> <name> not found in <where>; <what exists or the fix>`.
fn campaign_not_found(campaign: &str, state: &Path) -> String {
    format!(
        "Campaign {} not found in {}; af review campaigns lists the Campaigns, and af review run \
         --campaign {} starts it",
        terminal_text(campaign),
        state.display(),
        shell_words::quote(campaign)
    )
}

fn open_campaign_store(state: &Path, campaign: &str) -> Result<EventStore, String> {
    if !state.join("events.sqlite").exists() {
        return Err(campaign_not_found(campaign, state));
    }
    EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())
}

fn open_campaign_store_read_only(state: &Path, campaign: &str) -> Result<EventStore, String> {
    if !state.join("events.sqlite").exists() {
        return Err(campaign_not_found(campaign, state));
    }
    EventStore::open_read_only(state.join("events.sqlite")).map_err(|error| error.to_string())
}

/// A Campaign command's error, with the refusal named when another release wrote its Store.
fn explain_campaign(selector: &cli::CampaignSelector, error: String) -> String {
    match campaign_state(&selector.state, &selector.campaign) {
        Ok(state) => store_release::explain(&state, error),
        Err(_) => error,
    }
}

fn print_ledger(options: &LedgerOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let store = open_campaign_store_read_only(&state, &options.campaign)?;
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
            terminal_text(&finding.key),
            format!("{:?}", finding.severity).to_lowercase(),
            finding.status.as_str(),
            finding.convergence_scope_label(),
            finding
                .convergence_severity
                .map(|severity| format!("{severity:?}").to_lowercase())
                .unwrap_or_else(|| "-".to_string()),
            terminal_text(&finding.file),
            finding.line.map_or("-".to_string(), |l| l.to_string()),
            terminal_text(&finding.title)
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
                            terminal_text(&report.source),
                            report.round,
                            report.scope_label(),
                            terminal_text(&report.file),
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
        eprintln!("{}", terminal_lines(&task_summary));
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
    let mut lines = value.lines().map(terminal_text);
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
    let store = open_campaign_store_read_only(&state, &options.campaign)?;
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
    let store = open_campaign_store(&state, &options.campaign)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let ledger = LedgerProjection::rebuild(&store, &cas, &campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?
        .into_ledger();
    print_scope_authority_warnings(&ledger);
    let finding = ledger
        .finding_view(&options.key)
        .ok_or_else(|| format!("no finding with key {}", terminal_text(&options.key)))?;
    let run_id = campaign_run_id(&options.campaign);
    let head = current_campaign_head(&store, &cas, &run_id)?;
    let proposals = campaign_proposals(&store, &cas, &run_id)?;
    let (finding_ids, report_ids) = finding_claim_ids(&finding, &cas)?;

    println!(
        "{} [{}]",
        terminal_text(&finding.title),
        terminal_text(&finding.key)
    );
    if !finding.aliases.is_empty() {
        println!("aliases={}", terminal_text(&finding.aliases.join(",")));
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
        terminal_text(&finding.file),
        finding
            .line
            .map_or("-".to_string(), |line| line.to_string())
    );
    for (index, attached) in finding.reports.iter().enumerate() {
        println!(
            "\nreport {}: reviewer={} round={} severity={} scope={} location={}:{} id={}",
            index + 1,
            terminal_text(&attached.source),
            attached.round,
            format!("{:?}", attached.severity).to_lowercase(),
            attached.scope_label(),
            terminal_text(&attached.file),
            attached
                .line
                .map_or("-".to_string(), |line| line.to_string()),
            terminal_text(&attached.report_id)
        );
        let report = cas
            .get_json(&attached.report_id)
            .map_err(|e| format!("reading report {}: {e}", attached.report_id))?;
        // JSON escapes C0 controls itself; DEL and C1 controls it leaves raw, so each line is
        // shown through the helper too.
        let pretty = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        for line in pretty.lines() {
            println!("{}", terminal_text(line));
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
                .map(|note| format!(" - {}", one_line(note)))
                .unwrap_or_default()
        );
    }
    println!(
        "current note: {}",
        one_line(finding.current_note().unwrap_or("(none)"))
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
            println!("{}", proposal_line(proposal, &head));
        }
    }
    Ok(())
}

/// One Proposal of `af review show`. Its paths are model-authored, so each is shown through
/// [`terminal_text`]: a control character in one can neither drive the terminal nor forge a line.
fn proposal_line(proposal: &CampaignProposal, head: &str) -> String {
    format!(
        "  {} base={} applicability={} paths={}",
        terminal_text(&proposal.id),
        terminal_text(&proposal.proposal.base_snapshot_id),
        if proposal.proposal.base_snapshot_id == head {
            "current"
        } else {
            "stale"
        },
        proposal
            .proposal
            .paths
            .iter()
            .map(|path| terminal_text(path))
            .collect::<Vec<_>>()
            .join(",")
    )
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
    /// Who wrote the Campaign's Store, when another release did: the text lists such Campaigns
    /// in one row; the document keeps one problem each, its reason the named refusal.
    #[serde(skip)]
    release: Option<review_core::store_writer::StoreProvenance>,
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
    /// The newest Round started, as (Round, epoch), and whether no Run Report has closed it yet:
    /// the text table's ROUND and STATE, and the browser's. The document is unchanged.
    #[serde(skip)]
    latest_round: Option<(u32, u32)>,
    #[serde(skip)]
    round_open: bool,
}

impl CampaignView {
    /// `new` before its first Round, `open` while its newest Round has no Run Report, `closed`
    /// once one closed it.
    fn state_word(&self) -> &'static str {
        match (self.latest_round, self.round_open) {
            (None, _) => "new",
            (Some(_), true) => "open",
            (Some(_), false) => "closed",
        }
    }

    /// The Round as the table prints it: `2`, or `2.3` for its third epoch; `-` before one.
    fn round_label(&self) -> String {
        match self.latest_round {
            None => "-".to_owned(),
            Some((round, 1)) => round.to_string(),
            Some((round, epoch)) => format!("{round}.{epoch}"),
        }
    }

    /// What its Tasks charged in total, exactly as `af review report` counts each Task; `None`
    /// when no Task has run for it.
    fn chargeable_tokens(&self) -> Option<u128> {
        report_tasks::chargeable_tokens(&self.task_accounting)
    }

    /// The selector a command names it by: `--campaign LABEL`, and `--state DIR` unless its
    /// Store is the default one for that label under the default root.
    fn selector(&self, state: &Path, default_root: Option<&Path>) -> Vec<String> {
        let mut words = vec!["--campaign".to_owned(), self.label.clone()];
        let default = default_root.is_some_and(|root| state.parent() == Some(root))
            && self.state_dir == self.id;
        if !default {
            words.push("--state".to_owned());
            words.push(state.display().to_string());
        }
        words
    }
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
        CampaignsFormat::Text => print_campaigns_text(&view, &root, options.state_root.is_some()),
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
                    release: None,
                });
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            problems.push(CampaignProblemView {
                directory: directory_lossy,
                reason: "state-root entry is a symlink; Campaign enumeration does not follow it"
                    .to_string(),
                release: None,
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
                    release: None,
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
                release: None,
            });
            continue;
        }
        let state = match resolve_filesystem_path(&entry.path()) {
            Ok(state) => state,
            Err(reason) => {
                problems.push(CampaignProblemView {
                    directory: directory_lossy,
                    reason,
                    release: None,
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
                release: None,
            });
            continue;
        }
        let store = match EventStore::open_read_only(state.join("events.sqlite")) {
            Ok(store) => store,
            Err(error) => {
                problems.push(CampaignProblemView {
                    directory: directory_lossy,
                    reason: error.to_string(),
                    release: None,
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
                    release: None,
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
                release: None,
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
                release: None,
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
                    release: None,
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
                release: None,
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
                problems.push(CampaignProblemView {
                    directory,
                    reason,
                    release: None,
                });
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
    // A Campaign another release wrote is named by its writer, with the fix (ADR-0145).
    for problem in &mut problems {
        let state = root.join(&problem.directory);
        if let Some(provenance) = store_release::foreign(&state, &problem.reason) {
            problem.reason = store_release::refusal(&state, &provenance, "af review campaigns");
            problem.release = Some(provenance);
        }
    }
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
    let mut latest_round = None;
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
        latest_round = Some((started.round, started.epoch));
    }
    let round_authority = report_round_authority(&events)?;
    let reports = events
        .iter()
        .filter(|event| event.event_type.is_run_report())
        .collect::<Vec<_>>();
    let rounds = report_rounds(&reports, &round_authority)?;
    let round_open = latest_round.is_some_and(|(round, epoch)| {
        !rounds
            .iter()
            .any(|closed| closed.round == Some(round) && closed.epoch == Some(epoch))
    });
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
        latest_round,
        round_open,
    })
}

/// One Campaign Store directory read on its own, whatever its name: the Store a plain `af review
/// run` keeps under `review/local/`, or one `--state DIR` made, which the Store registry records.
/// The browser lists it with the entry `af review campaigns` would print for it. Nothing is
/// written, and a symlinked event log is refused as the enumeration refuses it.
fn read_campaign_dir(state: &Path) -> Result<CampaignView, String> {
    let database = state.join("events.sqlite");
    let metadata = std::fs::symlink_metadata(&database)
        .map_err(|error| format!("reading {}: {error}", database.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(
            "events.sqlite is not a real regular file beneath the Campaign state directory"
                .to_string(),
        );
    }
    let store = EventStore::open_read_only(&database).map_err(|error| error.to_string())?;
    let run_ids = store
        .run_ids()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|run_id| run_id.starts_with("campaign-"))
        .collect::<Vec<_>>();
    let [run_id] = run_ids.as_slice() else {
        return Err(format!(
            "state contains {} campaign runs; a Campaign state directory must contain exactly one",
            run_ids.len()
        ));
    };
    let label = run_id
        .strip_prefix("campaign-")
        .unwrap_or(run_id)
        .to_string();
    validate_campaign_component(&label)?;
    let id = campaign_id(&label);
    read_campaign_view(state, run_id, label, id, &store, false)
}

/// The `af review report --json` document of the Campaign `label` in the Store at `state`, for
/// the browser: read as `af review report` reads it, but silent, since a warning printed on
/// stderr would land on the browser's screen.
fn browse_report(state: &Path, label: &str) -> Result<serde_json::Value, String> {
    let store = open_campaign_store_read_only(state, label)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|error| error.to_string())?;
    let view = read_report_view_quietly(&store, &cas, label, false)?;
    serde_json::to_value(&view).map_err(|error| error.to_string())
}

fn print_campaigns_text(view: &CampaignListView, root: &Path, custom_root: bool) {
    let local = |unix_ms: u64| {
        let time = human::local_time(unix_ms);
        // The year is left out: the table is 100 columns, and an old Campaign's date says it.
        time.get(5..).unwrap_or(&time).to_owned()
    };
    print!("{}", campaigns_text(view, root, custom_root, &local));
}

/// The columns of `af review campaigns`, in order, and their widths; 100 columns with the two
/// spaces between each.
const CAMPAIGN_COLUMNS: [(&str, usize); 7] = [
    ("CAMPAIGN", 20),
    ("ROUND", 5),
    ("STATE", 6),
    ("VERDICT", 22),
    ("TOKENS", 9),
    ("WALL", 10),
    ("UPDATED", 16),
];

/// `text` in at most `width` characters, a cut marked with `~`.
fn clip_cell(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}~")
}

/// `af review campaigns` as text: a table of one row per Campaign (its label, newest Round,
/// state, verdict, what its Tasks charged and the wall-clock of its Rounds, and when its Store
/// was last written), in 100 columns, then the problems. `time` prints a Unix time. An empty
/// `custom_root` (`--state-root DIR`) names the command that starts a Campaign there: a plain
/// `af review run` would keep it under the default root instead.
fn campaigns_text(
    view: &CampaignListView,
    root: &Path,
    custom_root: bool,
    time: &dyn Fn(u64) -> String,
) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    if view.campaigns.is_empty() {
        let readable = if view.problems.is_empty() {
            ""
        } else {
            "readable "
        };
        let shown = terminal_text(&root.display().to_string());
        if custom_root {
            // A Store named by its label directly beneath the root is one this listing reads
            // (ADR-0035).
            let head = format!("No {readable}Campaigns under {shown}; start one there with");
            if head.chars().count() <= 100 {
                let _ = writeln!(text, "{head}");
            } else {
                // The path keeps a line of its own, whole, so it copies as it is.
                let _ = writeln!(
                    text,
                    "No {readable}Campaigns under\n  {shown}\nStart one there with"
                );
            }
            let state = root.join("NAME").display().to_string();
            let command = shell_words::join([
                "af",
                "review",
                "run",
                "--campaign",
                "NAME",
                "--state",
                state.as_str(),
            ]);
            for line in
                task_execution::runs::command_lines("  ", &terminal_text(&command), 100, "    ")
            {
                let _ = writeln!(text, "{line}");
            }
        } else {
            let _ = writeln!(
                text,
                "No {readable}Campaigns under {shown}; af review run --campaign NAME starts one"
            );
        }
    } else {
        let row = |cells: [String; 7]| {
            let mut line = String::new();
            for (index, (cell, (_, width))) in cells.iter().zip(CAMPAIGN_COLUMNS).enumerate() {
                if index > 0 {
                    line.push_str("  ");
                }
                let cell = clip_cell(cell, width);
                // The token count is a number: it lines up on the right.
                if index == 4 {
                    let _ = write!(line, "{cell:>width$}");
                } else {
                    let _ = write!(line, "{cell:<width$}");
                }
            }
            line.trim_end().to_owned()
        };
        let _ = writeln!(
            text,
            "{}",
            row(CAMPAIGN_COLUMNS.map(|(name, _)| name.into()))
        );
        for campaign in &view.campaigns {
            let line = row([
                terminal_text(&campaign.label),
                campaign.round_label(),
                campaign.state_word().to_owned(),
                terminal_text(campaign.verdict.as_deref().unwrap_or("-")),
                campaign
                    .chargeable_tokens()
                    .map_or("-".to_owned(), |tokens| tokens.to_string()),
                campaign.wall_ms.map_or("-".to_owned(), human_duration),
                campaign.last_activity_unix_ms.map_or("-".to_owned(), time),
            ]);
            let _ = writeln!(text, "{line}");
        }
        let _ = writeln!(
            text,
            "Details: af review report --campaign NAME; af review ledger --campaign NAME"
        );
    }
    if !view.problems.is_empty() {
        let _ = writeln!(text, "Problems: {}", view.problems.len());
    }
    for problem in view
        .problems
        .iter()
        .filter(|problem| problem.release.is_none())
    {
        // A problem is a sentence with its fix: it keeps its line, so a command in it copies
        // whole.
        let _ = writeln!(
            text,
            "  {}: {}",
            terminal_text(&problem.directory),
            terminal_text(&problem.reason)
        );
    }
    if let Some(row) = store_release::summary(
        view.problems
            .iter()
            .filter_map(|problem| problem.release.as_ref()),
        "af review campaigns",
    ) {
        for line in row.lines() {
            let _ = writeln!(text, "  {}", terminal_text(line));
        }
    }
    text
}

#[derive(serde::Serialize)]
struct ReviewReportView {
    schema: &'static str,
    campaign: String,
    runs_recorded: usize,
    ledger_round: u32,
    final_verdict: Option<String>,
    /// Why the final verdict is incomplete, when a model the bound one did not answer for is
    /// the reason: printed after it, never part of the document (ADR-0147).
    #[serde(skip)]
    final_reasons: Vec<String>,
    rounds: Vec<ReportRoundView>,
    /// Empty only for a Campaign whose first Task capture failed, which has no Rounds either.
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
fn wall_span_ms(rows: &[review_store::TaskAttemptWall]) -> Option<u64> {
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
    human::duration(ms)
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
    let store = open_campaign_store_read_only(&state, &options.campaign)?;
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
    read_report_view_quietly(store, cas, campaign, true)
}

/// The report document; `warn` prints the Ledger's Scope authority warnings on stderr, as every
/// command does and the browser does not.
fn read_report_view_quietly(
    store: &EventStore,
    cas: &Cas,
    campaign: &str,
    warn: bool,
) -> Result<ReviewReportView, String> {
    let run_id = campaign_run_id(campaign);
    let ledger = LedgerProjection::rebuild(store, cas, &run_id)
        .map_err(|e| e.to_string())?
        .into_ledger();
    if warn {
        print_scope_authority_warnings(&ledger);
    }
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
        schema: "af/review-report@4",
        campaign: campaign.into(),
        runs_recorded: reports.len(),
        ledger_round: ledger.round,
        final_verdict: reports
            .last()
            .map(|event| run_report(event).map(|report| render_verdict_v3(report.verdict)))
            .transpose()?,
        final_reasons: match reports.last().map(|event| run_report(event)).transpose()? {
            Some(RunReportPayloadV6 {
                verdict: RunVerdictV3::Incomplete { missing_nodes },
                ..
            }) => model_failures::missing_node_reasons(
                missing_nodes
                    .iter()
                    .map(|missing| (missing.node.as_str(), missing.reason.as_str())),
            ),
            _ => Vec::new(),
        },
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
    println!("Review campaign: {}", terminal_text(&report.campaign));
    println!("Runs recorded: {}", report.runs_recorded);
    println!("Ledger round: {}", report.ledger_round);
    println!(
        "Final verdict: {}",
        terminal_text(report.final_verdict.as_deref().unwrap_or("not recorded"))
    );
    for reason in &report.final_reasons {
        println!("  reason: {}", terminal_text(reason));
    }
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
            terminal_text(&round.verdict),
            round.tokens_label()
        );
    }
    // Node IDs in it are repository-authored: every line is shown through the helper.
    for line in report_tasks::render(&report.task_accounting, false).lines() {
        println!("{}", terminal_text(line));
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
            terminal_text(&demand.demand_id)
        );
        println!("    why: {}", one_line(&demand.why));
        println!(
            "    suggested method: {}",
            one_line(&demand.suggested_method)
        );
        println!("    source: {}", terminal_text(&demand.source));
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
            terminal_text(&finding.key),
            terminal_text(&finding.file),
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
    println!("# Review campaign `{}`", terminal_text(&report.campaign));
    println!();
    println!("- Runs recorded: {}", report.runs_recorded);
    println!("- Ledger round: {}", report.ledger_round);
    println!(
        "- Final verdict: {}",
        terminal_text(report.final_verdict.as_deref().unwrap_or("not recorded"))
    );
    for reason in &report.final_reasons {
        println!("  - reason: {}", markdown_line(reason));
    }
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
            terminal_text(&round.verdict),
            round.tokens_cell()
        );
    }
    println!();
    // Node IDs in it are repository-authored: every line is shown through the helper.
    for line in report_tasks::render(&report.task_accounting, true).lines() {
        println!("{}", terminal_text(line));
    }
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
            markdown_line(&demand.claim),
            terminal_text(&demand.demand_id)
        );
        println!("  - Why: {}", markdown_line(&demand.why));
        println!(
            "  - Suggested method: {}",
            markdown_line(&demand.suggested_method)
        );
        println!("  - Source: {}", terminal_text(&demand.source));
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
                one_line(&finding.title),
                terminal_text(&finding.key),
                terminal_text(&finding.file),
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
                        terminal_text(&attached.source),
                        attached.round,
                        attached.scope_label(),
                        terminal_text(&attached.file),
                        attached
                            .line
                            .map_or("-".to_string(), |line| line.to_string()),
                        terminal_text(&attached.report_id)
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

fn one_line(value: &str) -> String {
    terminal_text(&value.lines().collect::<Vec<_>>().join(" "))
}

/// A repository- or model-authored string made safe for text-mode output: every control
/// character (C0, DEL and C1, so ESC, BEL, a newline and a tab too) is shown as a visible
/// `\u{…}` escape instead of reaching the terminal, so a changed path or a finding title can
/// neither drive the terminal nor forge an output line. The control set is the one
/// `review_runner::cli::bounded_cli_line` removes; unlike that line, nothing here is dropped,
/// collapsed, bounded or withheld, so two distinct paths stay two distinct lines. `--json`
/// documents never pass through it: JSON escapes them itself.
pub(crate) fn terminal_text(value: &str) -> String {
    let mut shown = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_control() {
            shown.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
        } else {
            shown.push(c);
        }
    }
    shown
}

/// [`terminal_text`] for a message af composed that may span lines, an error above all: every
/// line goes through it, so no value interpolated into the message can drive the terminal, and
/// the message's own line breaks stay line breaks. A value that must not forge a line either is
/// passed through [`terminal_text`] where it is interpolated.
fn terminal_lines(message: &str) -> String {
    message
        .split('\n')
        .map(terminal_text)
        .collect::<Vec<_>>()
        .join("\n")
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
    one_line(value)
}

fn print_scope_authority_warnings(ledger: &Ledger) {
    for failure in ledger.scope_authority_failures() {
        match failure.authority {
            review_store::ScopeAuthorityKind::RoundBinding => eprintln!(
                "warning: round {} Report Scope is unknown: round binding disagrees for Subject {}: {}",
                failure.round,
                terminal_text(&failure.authority_id),
                terminal_text(&failure.reason)
            ),
            authority => eprintln!(
                "warning: round {} Report Scope is unknown: {:?} authority {} is unavailable: {}",
                failure.round,
                authority,
                terminal_text(&failure.authority_id),
                terminal_text(&failure.reason)
            ),
        }
    }
}

fn resolve(options: &ResolveOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state, &options.campaign)?;
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
    println!(
        "resolved {} -> {} ({resolution_id})",
        terminal_text(&options.key),
        now
    );
    Ok(())
}

fn attest_change(options: &AttestChangeOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state, &options.campaign)?;
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
        terminal_text(&options.finding_id)
    );
    Ok(())
}

fn verify_fix(options: &VerifyFixOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state, &options.campaign)?;
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
            terminal_text(&options.finding_id)
        ),
        None => println!(
            "verification for {} was negative ({verification}); Finding remains pending-verification",
            terminal_text(&options.finding_id)
        ),
    }
    Ok(())
}

fn challenge_resolution(options: &ChallengeResolutionOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state, &options.campaign)?;
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
    println!(
        "challenged {} -> contested ({id})",
        terminal_text(&options.finding_id)
    );
    Ok(())
}

fn advance_policy_time(options: &PolicyTimeOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state, &options.campaign)?;
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
    let mut store = open_campaign_store(&state, &options.campaign)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let mut ingest = Ingest::new(&mut store, &cas, run_id).map_err(|e| e.to_string())?;
    if undo {
        ingest
            .ungroup(&options.from, &options.into)
            .map_err(|e| e.to_string())?;
        println!(
            "ungrouped {} from {}",
            terminal_text(&options.from),
            terminal_text(&options.into)
        );
    } else {
        ingest
            .group(&options.from, &options.into)
            .map_err(|e| e.to_string())?;
        println!(
            "grouped {} into {}",
            terminal_text(&options.from),
            terminal_text(&options.into)
        );
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
    let mut store = open_campaign_store(&state, &options.campaign)?;
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
    println!(
        "evidence {evidence_id} recorded for {}",
        terminal_text(&options.demand_id)
    );
    Ok(())
}

fn satisfy_evidence(options: &EvidenceSatisfyOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state, &options.campaign)?;
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
        terminal_text(&options.demand_id),
        terminal_text(&options.evidence_id),
        terminal_text(&options.policy_revision)
    );
    if let Some(reuse_id) = reuse_id {
        println!("future-Subject Evidence reuse admitted ({reuse_id})");
    }
    Ok(())
}

fn waive_demand(options: &DemandWaiveOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let mut store = open_campaign_store(&state, &options.campaign)?;
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
    println!(
        "demand {} waived ({waiver_id})",
        terminal_text(&options.demand_id)
    );
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
        // Every Ledger port is typed, so the Finding Set is selected by its port's declared type
        // and never by sniffing a sibling DemandSet@1 artifact's content.
        for artifact_id in receipt
            .outputs
            .iter()
            .filter(|port| port.artifact_type == review_core::contract::FINDING_SET_V1)
            .flat_map(|port| port.artifact_ids.iter())
        {
            let value = cas
                .get_json(artifact_id)
                .map_err(|error| error.to_string())?;
            let envelope: review_core::ArtifactEnvelope =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
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
    storage::owned::prepare(&state, options.state.is_some())?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|error| error.to_string())?;
    let git_home = state.join("git-home");
    storage::owned::private_dir(&git_home)?;
    let repo = Repo::open(&options.repo, &git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let campaign = campaign_run_id(options.campaign.as_deref().unwrap_or("local"));
    review_task::require_common_campaign(&cas, &store, &campaign)?;
    review_task::doctor(options, &cas, &mut store, &repo, &campaign)
}

/// Remove the sandbox directories that earlier `af` processes left in `$TMPDIR` when they were
/// killed. A sandbox is removed by the handle that owns it, so only a process that never ran
/// its drops leaves one, and a leftover tree a Gate built into holds a whole `target/`. The
/// sweep runs for the commands that create sandboxes themselves (`runs_task_work`), on its own
/// thread so a long removal overlaps the command's work, and `main` joins it before exiting so
/// it always completes. It keeps every directory whose process still exists and every one the
/// kernel preserved on purpose. One stderr line reports what was removed; stdout and every
/// `--json` document are untouched.
/// The sweep's line, if it removed anything, is printed once the command's own work is done:
/// the first line of `af task run` is the one that says what the run does (ADR-0209).
fn sweep_stale_sandboxes_in_background() -> Option<std::thread::JoinHandle<Option<String>>> {
    std::thread::Builder::new()
        .name("af-sandbox-sweep".into())
        .spawn(|| {
            let report = review_sandbox::sweep_stale_sandboxes();
            (report.removed > 0).then(|| {
                format!(
                    "af: removed {} stale sandbox director{} left by exited processes under {}",
                    report.removed,
                    if report.removed == 1 { "y" } else { "ies" },
                    terminal_text(&review_sandbox::sandbox_root().display().to_string())
                )
            })
        })
        .ok()
}

fn run(options: &Options) -> Result<RunVerdict, String> {
    let state = options.resolved_state_dir()?;
    storage::owned::prepare(&state, options.state.is_some())?;
    // Held for the whole run: no sweep and no `af review gc` removes this campaign under it.
    // A run that cannot hold it does not start: unprotected, a sweep could remove its Store.
    let _running = storage::hold_running(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|error| error.to_string())?;
    let git_home = state.join("git-home");
    storage::owned::private_dir(&git_home)?;
    let repo = Repo::open(&options.repo, &git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let campaign = campaign_run_id(options.campaign.as_deref().unwrap_or("local"));
    review_task::require_common_campaign(&cas, &store, &campaign)?;
    review_selector_refusal(options, "run")?;
    review_task::run(options, &cas, &mut store, &repo, &campaign)
}

#[cfg(test)]
mod terminal_text_tests {
    use super::{
        CampaignProposal, markdown_line, one_line, proposal_line, terminal_lines, terminal_text,
    };

    /// A message af composed keeps its own line breaks; every line of it is shown through
    /// `terminal_text`, so a value interpolated into it sends no control character (audit C3).
    #[test]
    fn a_message_keeps_its_lines_and_sends_no_control_character() {
        assert_eq!(
            terminal_lines("opening esc\x1b]0;PWNED\x07repo: gone\n  try: af --help\r"),
            "opening esc\\u{1b}]0;PWNED\\u{7}repo: gone\n  try: af --help\\u{d}"
        );
        assert_eq!(terminal_lines("plain"), "plain");
        assert_eq!(terminal_lines(""), "");
    }

    #[test]
    fn every_control_character_is_shown_not_sent() {
        assert_eq!(
            terminal_text("esc\x1b]0;PWNED\x07name"),
            "esc\\u{1b}]0;PWNED\\u{7}name"
        );
        assert_eq!(
            terminal_text("new\nline\ttab\r"),
            "new\\u{a}line\\u{9}tab\\u{d}"
        );
        assert_eq!(
            terminal_text("del\x7f c1\u{9b}31m"),
            "del\\u{7f} c1\\u{9b}31m"
        );
        // Printable text, Unicode included, is untouched: nothing is dropped or bounded.
        let plain = "src/caf\u{e9}.rs \u{2014} a title with `code` and 100% of it";
        assert_eq!(terminal_text(plain), plain);
        let long = "x".repeat(10_000);
        assert_eq!(terminal_text(&long), long);
        for value in ["esc\x1b[2J", "bell\x07", "a\nb", "c1\u{85}", "\x00"] {
            for shown in [terminal_text(value), one_line(value), markdown_line(value)] {
                assert!(!shown.chars().any(char::is_control), "{shown:?}");
            }
        }
        assert_eq!(one_line("two\nlines\x1b"), "two lines\\u{1b}");
    }

    /// A Proposal's paths are model-authored: `af review show` prints one holding ESC, BEL or a
    /// newline escaped, on its one line.
    #[test]
    fn a_proposal_path_is_shown_not_sent() {
        let proposal = CampaignProposal {
            id: "sha256:proposal".into(),
            proposal: review_core::PatchProposal {
                base_snapshot_id: "sha256:head".into(),
                patch_artifact_id: "sha256:patch".into(),
                finding_refs: Vec::new(),
                evidence_ids: Vec::new(),
                paths: vec![
                    "esc\x1b]0;PWNED\x07name".into(),
                    "new\n  sha256:forged base=x applicability=current paths=y".into(),
                ],
                description: String::new(),
                auto_apply_nominated: false,
            },
        };
        let line = proposal_line(&proposal, "sha256:head");
        assert!(!line.chars().any(char::is_control), "{line:?}");
        assert_eq!(line.lines().count(), 1, "{line}");
        assert_eq!(
            line,
            "  sha256:proposal base=sha256:head applicability=current \
             paths=esc\\u{1b}]0;PWNED\\u{7}name,new\\u{a}  sha256:forged base=x \
             applicability=current paths=y"
        );
    }
}

#[cfg(test)]
mod option_tests {
    /// Every command that runs Task Workers takes over SIGINT and SIGTERM and holds the Storage
    /// Budget; `af self optimize --execute` is one (issue #158), its preview is not.
    #[test]
    fn the_commands_that_run_task_work_are_exactly_these() {
        use clap::Parser;
        let table: &[(&[&str], bool, bool)] = &[
            (&["task", "run", "T"], true, false),
            (&["task", "run", "T", "--verbose"], true, true),
            (
                &["task", "start", "--file", "t.json", "--execute"],
                true,
                false,
            ),
            (
                &[
                    "task",
                    "start",
                    "--file",
                    "t.json",
                    "--execute",
                    "--verbose",
                ],
                true,
                true,
            ),
            (&["task", "start", "--file", "t.json"], false, false),
            (&["review", "run", "--campaign", "c"], true, false),
            // P4 made `--campaign` required: a doctor's admission evidence is a Campaign's.
            (&["provider", "doctor", "--campaign", "c"], true, false),
            (&["self", "optimize", "--execute"], true, false),
            (&["self", "optimize"], false, false),
            (&["self", "status"], false, false),
            (&["self", "update", "--check"], false, false),
            (&["storage", "prune", "--apply"], false, false),
            (&["task", "list"], false, false),
        ];
        for (words, runs, verbose) in table {
            let parsed =
                crate::cli::Af::try_parse_from(std::iter::once("af").chain(words.iter().copied()))
                    .unwrap_or_else(|error| panic!("{words:?}: {error}"));
            let command = parsed.command.unwrap();
            assert_eq!(super::runs_task_work(&command), *runs, "{words:?}");
            assert_eq!(
                super::needs_interrupt_handling(&command),
                *runs,
                "{words:?}"
            );
            assert_eq!(super::verbose_sweeps(&command), *verbose, "{words:?}");
        }
        // --verbose belongs to the run of `af task start`, not to its preview.
        assert!(
            crate::cli::Af::try_parse_from(["af", "task", "start", "--file", "t", "--verbose"])
                .is_err()
        );
    }

    #[test]
    fn private_login_handles_interrupts_without_starting_task_maintenance() {
        use clap::Parser;
        let parsed = crate::cli::Af::parse_from([
            "af",
            "provider",
            "auth",
            "begin",
            "codex-main",
            "--kind",
            "codex",
            "--auth-dir",
            "/fixture/auth",
            "--host-read-fd",
            "3",
            "--host-write-fd",
            "4",
        ]);
        let command = parsed.command.unwrap();
        assert!(super::needs_interrupt_handling(&command));
        assert!(!super::runs_task_work(&command));
    }

    use super::{
        CampaignListView, CampaignMode, CampaignProblemView, Options, browse_report, campaign_id,
        campaign_labels_beneath, campaign_state_beneath, campaigns_text, default_campaigns_root,
        enumerate_campaigns, latest_round_evidence, read_campaign_dir, report_tasks,
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

    pub(crate) fn campaign_manifest(
        cas: &review_store::Cas,
        ledger_node: &str,
    ) -> (String, String, String) {
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

    /// A Campaign Store at `state` holding the opening of Campaign `label` and, for each
    /// `(round, epoch)` of `rounds`, a Round started and never closed: what the browser's
    /// Campaigns pane lists as an open Round.
    pub(crate) fn write_campaign_rounds(
        state: &std::path::Path,
        label: &str,
        rounds: &[(u32, u32)],
    ) {
        write_campaign_opening(state, label);
        let cas = review_store::Cas::open(state.join("cas")).unwrap();
        let (manifest_id, findings, demands) = campaign_manifest(&cas, "ledger");
        let subject = cas.put(b"subject").unwrap();
        let mut store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
        for (round, epoch) in rounds {
            let started = review_core::RoundStartedPayloadV1 {
                round: *round,
                epoch: *epoch,
                campaign_manifest_id: manifest_id.clone(),
                subject_id: subject.clone(),
                prior_finding_set_id: findings.clone(),
                prior_demand_set_id: demands.clone(),
            };
            store
                .append(
                    &super::campaign_run_id(label),
                    &cas,
                    review_store::NewEvent::new(
                        review_core::EventType::RoundStartedV1,
                        serde_json::to_value(started).unwrap(),
                    )
                    .referencing(vec![
                        manifest_id.clone(),
                        subject.clone(),
                        findings.clone(),
                        demands.clone(),
                    ]),
                )
                .unwrap();
        }
    }

    pub(crate) fn write_campaign_opening(state: &std::path::Path, label: &str) {
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

    /// `af review campaigns` prints one scannable row per Campaign in 100 columns: its label,
    /// newest Round, state, verdict, spend and last write; the problems follow, those another
    /// release wrote as one row. The `--format json` document keeps its fields as they were.
    #[test]
    fn campaigns_text_is_a_table_of_one_row_per_campaign_in_100_columns() {
        const TABLE: &str = include_str!("../tests/fixtures/review/campaigns-100.txt");
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path())
            .unwrap()
            .join("campaigns");
        let long = "a-label-longer-than-its-column";
        write_campaign_rounds(&root.join(campaign_id("pr-42")), "pr-42", &[(1, 1)]);
        write_campaign_rounds(&root.join(campaign_id(long)), long, &[(1, 1)]);
        write_campaign_opening(&root.join(campaign_id("fresh")), "fresh");
        let mut campaigns = enumerate_campaigns(&root, false).unwrap().campaigns;
        let labels: Vec<&str> = campaigns.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, [long, "fresh", "pr-42"]);
        assert_eq!(
            campaigns
                .iter()
                .map(|c| (c.state_word(), c.round_label()))
                .collect::<Vec<_>>(),
            [
                ("open", "1".into()),
                ("new", "-".into()),
                ("open", "1".into())
            ]
        );
        // What a fixture Store cannot record without running reviewers: a closed Round's
        // verdict, the Tasks' charge, the wall-clock and a fixed last write.
        for (index, campaign) in campaigns.iter_mut().enumerate() {
            campaign.last_activity_unix_ms = Some(1_792_735_979_000 + index as u64 * 60_000);
        }
        campaigns[0].verdict = Some("incomplete (2 missing nodes)".into());
        campaigns[0].latest_round = Some((2, 3));
        campaigns[0].round_open = false;
        campaigns[0].task_accounting = vec![report_tasks::charged("review", 1_234_567)];
        campaigns[0].wall_ms = Some(185_000);
        campaigns[2].verdict = Some("fail (not_converged)".into());
        campaigns[2].round_open = false;
        campaigns[2].task_accounting = vec![
            report_tasks::charged("review", 40_000),
            report_tasks::charged("review-2", 2_345),
        ];
        campaigns[2].wall_ms = Some(4_250);
        let problems = vec![
            CampaignProblemView {
                directory: "broken".into(),
                reason: "events.sqlite is not a real regular file beneath the Campaign state \
                         directory"
                    .into(),
                release: None,
            },
            CampaignProblemView {
                directory: "old-pr".into(),
                reason: "Store old-pr was written by af 99.1.0".into(),
                release: Some(review_core::store_writer::StoreProvenance::OtherReleases(
                    vec!["99.1.0".into()],
                )),
            },
        ];
        let view = CampaignListView {
            schema: "af/review-campaigns@1",
            campaigns,
            problems,
        };
        let utc = |unix_ms: u64| crate::human::utc_time(unix_ms)[5..].to_owned();
        let text = campaigns_text(&view, &root, false, &utc);
        assert_eq!(text, TABLE);
        let table = text
            .lines()
            .take_while(|line| !line.starts_with("Problems:"));
        assert!(table.clone().all(|line| line.len() <= 100), "{text}");
        assert_eq!(
            table.count(),
            5,
            "a header, a row per Campaign, the details line"
        );
        // The document is what it was: the table's columns are not new fields of it.
        let document = serde_json::to_value(&view).unwrap();
        let first = document["campaigns"][0].as_object().unwrap();
        for absent in ["latest_round", "round_open", "task_accounting", "state"] {
            assert!(!first.contains_key(absent), "{absent}");
        }
        assert_eq!(document["problems"].as_array().unwrap().len(), 2);
        // Nothing listed: one line that says where, and how to start one.
        let empty = CampaignListView {
            schema: "af/review-campaigns@1",
            campaigns: Vec::new(),
            problems: Vec::new(),
        };
        let text = campaigns_text(&empty, &root, false, &utc);
        assert_eq!(
            text,
            format!(
                "No Campaigns under {}; af review run --campaign NAME starts one\n",
                root.display()
            )
        );
    }

    /// An empty `--state-root R` names the command that starts a Campaign under R, in 100
    /// columns; the Store that command names is one the same listing then shows.
    #[test]
    fn an_empty_custom_root_names_a_command_that_starts_a_campaign_there() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path())
            .unwrap()
            .join("campaigns");
        std::fs::create_dir_all(&root).unwrap();
        let empty = CampaignListView {
            schema: "af/review-campaigns@1",
            campaigns: Vec::new(),
            problems: Vec::new(),
        };
        let utc = |unix_ms: u64| crate::human::utc_time(unix_ms)[5..].to_owned();
        let text = campaigns_text(&empty, &root, true, &utc);
        // A root wider than the terminal (a long `TMPDIR`) is one unbreakable word: it keeps a
        // line of its own, alone or as the value of its `--state`.
        for line in text.lines() {
            let unit = line.trim().trim_end_matches('\\').trim_end();
            let alone = match shell_words::split(unit).as_deref() {
                Ok([_]) => true,
                Ok([flag, _]) => flag.starts_with("--"),
                _ => false,
            };
            assert!(line.chars().count() <= 100 || alone, "{line}");
        }
        let lines: Vec<&str> = text.lines().collect();
        // The sentence on one line where the root fits, else the root on a line of its own.
        let head = format!(
            "No Campaigns under {}; start one there with",
            root.display()
        );
        if head.len() <= 100 {
            assert_eq!(lines[0], head, "{text}");
        } else {
            assert_eq!(lines[1], format!("  {}", root.display()), "{text}");
        }
        let at = lines
            .iter()
            .position(|line| line.starts_with("  af review run"))
            .unwrap_or_else(|| panic!("{text}"));
        let command: String = lines[at..]
            .iter()
            .map(|line| line.trim().trim_end_matches('\\').trim())
            .collect::<Vec<_>>()
            .join(" ");
        let words = shell_words::split(&command).unwrap();
        assert_eq!(
            words[..5],
            ["af", "review", "run", "--campaign", "NAME"],
            "{text}"
        );
        assert_eq!(words[5], "--state", "{text}");
        assert_eq!(
            std::path::PathBuf::from(&words[6]),
            root.join("NAME"),
            "{text}"
        );
        // A root too long for the sentence keeps a line of its own.
        let long = root.join("x".repeat(80));
        let text_long = campaigns_text(&empty, &long, true, &utc);
        assert!(
            text_long.starts_with(&format!(
                "No Campaigns under\n  {}\nStart one there with\n",
                long.display()
            )),
            "{text_long}"
        );
        // What that command writes, a Campaign `NAME` in `R/NAME`, is what the listing reads.
        let label = "pr-7";
        let state = std::path::PathBuf::from(words[6].replace("NAME", label));
        write_campaign_rounds(&state, label, &[(1, 1)]);
        let listed = enumerate_campaigns(&root, false).unwrap();
        assert!(listed.problems.is_empty(), "{:?}", listed.problems.len());
        assert_eq!(listed.campaigns.len(), 1);
        assert_eq!(listed.campaigns[0].label, label);
    }

    /// A Campaign Store read on its own, whatever its directory is called: the Store of a plain
    /// `af review run` under `review/local/`, or a registered `--state DIR`.
    #[test]
    fn a_campaign_store_is_read_whatever_its_directory_is_called() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("review/local/0123456789abcdef");
        write_campaign_rounds(&state, "local", &[(1, 1)]);
        let view = read_campaign_dir(&state).unwrap();
        assert_eq!((view.label.as_str(), view.state_word()), ("local", "open"));
        assert_eq!(view.latest_round, Some((1, 1)));
        // Its commands name the Store: the default root does not resolve it by its label.
        let selector = view.selector(&state, Some(&temp.path().join("campaigns")));
        assert_eq!(
            selector,
            [
                "--campaign",
                "local",
                "--state",
                &state.display().to_string()
            ]
        );
        // The report document the browser reads, without a word on stderr.
        let report = browse_report(&state, "local").unwrap();
        assert_eq!(report["schema"], "af/review-report@4");
        assert_eq!(report["campaign"], "local");
        // A directory without a Store is no Campaign.
        let Err(error) = read_campaign_dir(&temp.path().join("review")) else {
            panic!("a directory without a Store was read as a Campaign");
        };
        assert!(error.contains("events.sqlite"), "{error}");
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

    /// A label that a listing could not show truthfully is refused: too long to show whole, or
    /// holding a character that is invisible or reorders the text around it.
    #[test]
    fn campaign_labels_are_bounded_and_refuse_invisible_or_reordering_characters() {
        assert!(validate_campaign_name(&"a".repeat(128)).is_ok());
        assert!(validate_campaign_name(&"é".repeat(64)).is_ok());
        let error = validate_campaign_name(&"a".repeat(129)).unwrap_err();
        assert!(error.contains("at most 128 bytes"), "{error}");
        assert!(validate_campaign_name(&"é".repeat(65)).is_err());
        for hostile in [
            "audit\u{2028}approved",
            "audit\u{2029}approved",
            "audit\u{202E}devorppa",
            "audit\u{200B}",
            "\u{2066}isolate\u{2069}",
            "soft\u{00AD}hyphen",
            "bom\u{FEFF}",
            "tag\u{E0041}",
        ] {
            let error = validate_campaign_name(hostile).unwrap_err();
            assert!(error.contains("U+"), "{hostile:?}: {error}");
            assert!(
                !error.contains(['\u{2028}', '\u{2029}', '\u{202E}']),
                "{error}"
            );
        }
        assert!(validate_campaign_name("révision-ünicode-中文").is_ok());
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
