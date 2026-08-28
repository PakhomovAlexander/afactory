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
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use review_attempt::{Budget, BudgetLedger, Scope};
use review_core::{
    EventType, RunFailureReasonV2, RunFailureReasonV3, RunReportPayloadV2, RunReportPayloadV3,
    RunVerdictV2, RunVerdictV3, Severity,
};
use review_graph::NodeOutcome;
use review_pipeline::{Kernel, RunVerdict};
use review_runner::ReviewerAdapter;
use review_source_git::Repo;
use review_store::{Cas, EventStore, Ingest, Ledger, LedgerProjection, Status, Verdict};
use sha2::{Digest, Sha256};

mod authority;
mod onboard;
mod providers;
mod task;
mod tui;

#[derive(Clone)]
struct Options {
    repo: PathBuf,
    pipeline: PathBuf,
    state: Option<PathBuf>,
    campaign: Option<String>,
    focus: Option<String>,
    authority: Option<String>,
    uncommitted: bool,
    restart_round: bool,
    timeout: Option<Duration>,
    git_timeout: Option<Duration>,
    provider_bindings: BTreeMap<String, String>,
    provider_resumes: BTreeMap<String, u64>,
    json: bool,
}

impl Options {
    /// Resolve state once for both execution and presentation. Relative paths use the process
    /// working directory, preserving the CLI's historical meaning, and repository-contained
    /// state is confined to the review tree's `runs` directory.
    fn resolved_state_dir(&self) -> Result<PathBuf, String> {
        if let Some(campaign) = &self.campaign {
            validate_campaign_name(campaign)?;
        }
        let requested = match (&self.state, &self.campaign) {
            (Some(state), _) => state.clone(),
            (None, Some(campaign)) => default_campaign_state(campaign)?,
            (None, None) => default_local_state(&self.repo)?,
        };
        let state = resolve_filesystem_path(&requested)?;
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
    validate_campaign_name(campaign)?;
    Ok(xdg_state_root()?.join("af/review/campaigns").join(campaign))
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
    let mut components = Path::new(campaign).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(format!(
            "campaign name `{campaign}` must be one safe path component"
        ));
    }
    Ok(())
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

fn resolve_codex_home(home: &str, configured: Option<&OsStr>) -> Result<String, String> {
    match configured {
        Some(value) if value.is_empty() => Err("CODEX_HOME is empty".to_string()),
        Some(value) => value
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| "CODEX_HOME must be valid UTF-8".to_string()),
        None => Ok(format!("{home}/.codex")),
    }
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

struct ReportOptions {
    state: Option<PathBuf>,
    campaign: String,
    format: String,
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

fn usage() -> ! {
    eprintln!(
        "usage: af review [run]   [--repo DIR] [--pipeline FILE] [--state DIR] \
         [--campaign NAME] [--authority REV] [--uncommitted] [--restart-round] [--focus TEXT] [--timeout-secs N] [--git-timeout-secs N]\n\
        \x20                       [--provider NODE=PROVIDER_ID] [--resume-provider OPERATION_ID:EPOCH] [--json]\n\
        \x20      af review tui     [--repo DIR] [--pipeline FILE] [--state DIR] \
         [--campaign NAME] [--authority REV] [--uncommitted] [--restart-round] [--focus TEXT] [--timeout-secs N] [--git-timeout-secs N]\n\
        \x20      af review ledger  --campaign NAME [--state DIR] [--long]\n\
        \x20      af review show    --campaign NAME [--state DIR] KEY\n\
        \x20      af review report  --campaign NAME [--state DIR] [--format md]\n\
        \x20      af review resolve --campaign NAME [--state DIR] KEY rejected|wontfix-tracked --policy REV --reason TEXT [--actor ACTOR] [--evidence ID]...\n\
        \x20      af review attest-change --campaign NAME [--state DIR] FINDING --region PATH[:START-END] --reason TEXT [--actor ACTOR] [--evidence ID]...\n\
        \x20      af review verify-fix --campaign NAME [--state DIR] FINDING ATTESTATION --policy REV --reason TEXT (--positive|--negative) [--verifier ACTOR] [--evidence ID]...\n\
        \x20      af review challenge-resolution --campaign NAME [--state DIR] FINDING --kind new-evidence|higher-severity|outside-scope|expired --reason TEXT [--actor ACTOR] [--evidence ID]...\n\
        \x20      af review policy-time advance --campaign NAME [--state DIR] TICK --reason TEXT [--actor ACTOR]\n\
        \x20      af review group   --campaign NAME [--state DIR] FROM INTO\n\
        \x20      af review ungroup --campaign NAME [--state DIR] FROM INTO\n\
        \x20      af review evidence add --campaign NAME [--state DIR] DEMAND FILE [--actor ACTOR]\n\
        \x20      af review evidence satisfy --campaign NAME [--state DIR] DEMAND EVIDENCE --policy REV --reason TEXT [--admit-reuse] [--actor ACTOR]\n\
        \x20      af review demand waive --campaign NAME [--state DIR] DEMAND --policy REV --reason TEXT [--actor ACTOR]\n\
        \x20      af provider status\n\
        \x20      af onboard [--repo DIR] [--runner mixed|claude|codex] [--gate NAME=COMMAND]... [--apply|--refresh-lock] [--json]\n\
        \x20      af task start --kind implement --goal TEXT [--repo DIR] [--pipeline FILE] [--state DIR] [--authority REV|--uncommitted] [--timeout-secs N] [--json]\n\
        \x20      af task deliver TASK_ID --repo DIR --branch NAME --worktree DIR --confirm TASK_ID [--state DIR] [--json]\n\
        \x20      af task list [--repo DIR] [--state DIR] [--json]\n\
        \x20      af task show TASK_ID [--repo DIR] [--state DIR] [--json]\n\
        \x20      af --version\n\
         \n\
         wontfix-tracked also requires --max-severity, --tracking, and --expires-at-policy-time"
    );
    std::process::exit(2);
}

fn campaign_run_id(campaign: &str) -> String {
    format!("campaign-{campaign}")
}

fn campaign_state(state: &Option<PathBuf>, campaign: &str) -> Result<PathBuf, String> {
    validate_campaign_name(campaign)?;
    let requested = match state {
        Some(state) => state.clone(),
        None => default_campaign_state(campaign)?,
    };
    resolve_filesystem_path(&requested)
}

fn parse_run(mut args: impl Iterator<Item = String>) -> Options {
    let mut options = Options {
        repo: PathBuf::from("."),
        pipeline: PathBuf::from(".af/pipelines/review.toml"),
        state: None,
        campaign: None,
        focus: None,
        authority: None,
        uncommitted: false,
        restart_round: false,
        timeout: None,
        git_timeout: None,
        provider_bindings: std::collections::BTreeMap::new(),
        provider_resumes: std::collections::BTreeMap::new(),
        json: false,
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--repo" => options.repo = PathBuf::from(value()),
            "--pipeline" => options.pipeline = PathBuf::from(value()),
            "--state" => options.state = Some(PathBuf::from(value())),
            "--campaign" => options.campaign = Some(value()),
            "--focus" => options.focus = Some(value()),
            "--authority" => options.authority = Some(value()),
            "--uncommitted" => options.uncommitted = true,
            "--restart-round" => options.restart_round = true,
            "--json" => options.json = true,
            "--timeout-secs" => {
                options.timeout = Some(Duration::from_secs(
                    value().parse().unwrap_or_else(|_| usage()),
                ))
            }
            "--git-timeout-secs" => {
                options.git_timeout = Some(Duration::from_secs(
                    value().parse().unwrap_or_else(|_| usage()),
                ))
            }
            "--provider" => {
                let binding = value();
                let (node, provider) = binding.split_once('=').unwrap_or_else(|| usage());
                if node.is_empty()
                    || provider.is_empty()
                    || options
                        .provider_bindings
                        .insert(node.to_string(), provider.to_string())
                        .is_some()
                {
                    usage();
                }
            }
            "--resume-provider" => {
                let token = value();
                let (operation, epoch) = token.rsplit_once(':').unwrap_or_else(|| usage());
                let epoch = epoch.parse::<u64>().unwrap_or_else(|_| usage());
                if operation.len() != 26
                    || !operation
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                    || epoch == 0
                    || options
                        .provider_resumes
                        .insert(operation.to_string(), epoch)
                        .is_some()
                {
                    usage();
                }
            }
            _ => usage(),
        }
    }
    options
}

fn parse_ledger(mut args: std::env::Args) -> LedgerOptions {
    let mut state = None;
    let mut campaign = None;
    let mut long = false;
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--long" => long = true,
            _ => usage(),
        }
    }
    LedgerOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        long,
    }
}

fn parse_show(mut args: std::env::Args) -> ShowOptions {
    let mut state = None;
    let mut campaign = None;
    let mut key = None;
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            other if !other.starts_with("--") && key.is_none() => key = Some(other.to_string()),
            _ => usage(),
        }
    }
    ShowOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        key: key.unwrap_or_else(|| usage()),
    }
}

fn parse_report(mut args: std::env::Args) -> ReportOptions {
    let mut state = None;
    let mut campaign = None;
    let mut format = "md".to_string();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--format" => format = value(),
            _ => usage(),
        }
    }
    if format != "md" {
        usage();
    }
    ReportOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        format,
    }
}

fn parse_resolve(mut args: std::env::Args) -> ResolveOptions {
    let mut state = None;
    let mut campaign = None;
    let mut actor = None;
    let mut policy_revision = None;
    let mut reason = None;
    let mut evidence_ids = Vec::new();
    let mut max_accepted_severity = None;
    let mut tracking_reference = None;
    let mut expires_at_policy_time = None;
    let mut positional: Vec<String> = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--actor" => actor = Some(value()),
            "--policy" => policy_revision = Some(value()),
            "--reason" => reason = Some(value()),
            "--evidence" => evidence_ids.push(value()),
            "--max-severity" => {
                max_accepted_severity = Some(match value().as_str() {
                    "minor" => Severity::Minor,
                    "major" => Severity::Major,
                    "blocker" => Severity::Blocker,
                    _ => usage(),
                })
            }
            "--tracking" => tracking_reference = Some(value()),
            "--expires-at-policy-time" => {
                expires_at_policy_time = Some(value().parse().unwrap_or_else(|_| usage()))
            }
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let (Some(campaign), [key, status]) = (campaign, positional.as_slice()) else {
        usage()
    };
    ResolveOptions {
        state,
        campaign,
        key: key.clone(),
        status: status.clone(),
        actor,
        policy_revision: policy_revision.unwrap_or_else(|| usage()),
        reason: reason.unwrap_or_else(|| usage()),
        evidence_ids,
        max_accepted_severity,
        tracking_reference,
        expires_at_policy_time,
    }
}

fn parse_changed_region(value: &str) -> review_core::ChangedRegionV1 {
    let Some((path, lines)) = value.rsplit_once(':') else {
        return review_core::ChangedRegionV1 {
            path: value.into(),
            start_line: None,
            end_line: None,
        };
    };
    let Some((start, end)) = lines.split_once('-') else {
        usage()
    };
    review_core::ChangedRegionV1 {
        path: path.into(),
        start_line: Some(start.parse().unwrap_or_else(|_| usage())),
        end_line: Some(end.parse().unwrap_or_else(|_| usage())),
    }
}

fn parse_attest_change(mut args: std::env::Args) -> AttestChangeOptions {
    let mut state = None;
    let mut campaign = None;
    let mut actor = None;
    let mut reason = None;
    let mut regions = Vec::new();
    let mut evidence_ids = Vec::new();
    let mut positional = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--actor" => actor = Some(value()),
            "--reason" => reason = Some(value()),
            "--region" => regions.push(parse_changed_region(&value())),
            "--evidence" => evidence_ids.push(value()),
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let [finding_id] = positional.as_slice() else {
        usage()
    };
    AttestChangeOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        finding_id: finding_id.clone(),
        actor,
        reason: reason.unwrap_or_else(|| usage()),
        regions,
        evidence_ids,
    }
}

fn parse_verify_fix(mut args: std::env::Args) -> VerifyFixOptions {
    let mut state = None;
    let mut campaign = None;
    let mut verifier = None;
    let mut policy_revision = None;
    let mut reason = None;
    let mut positive = None;
    let mut evidence_ids = Vec::new();
    let mut positional = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--verifier" => verifier = Some(value()),
            "--policy" => policy_revision = Some(value()),
            "--reason" => reason = Some(value()),
            "--positive" if positive.is_none() => positive = Some(true),
            "--negative" if positive.is_none() => positive = Some(false),
            "--evidence" => evidence_ids.push(value()),
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let [finding_id, attestation_id] = positional.as_slice() else {
        usage()
    };
    VerifyFixOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        finding_id: finding_id.clone(),
        attestation_id: attestation_id.clone(),
        verifier,
        policy_revision: policy_revision.unwrap_or_else(|| usage()),
        reason: reason.unwrap_or_else(|| usage()),
        positive: positive.unwrap_or_else(|| usage()),
        evidence_ids,
    }
}

fn parse_challenge_resolution(mut args: std::env::Args) -> ChallengeResolutionOptions {
    let mut state = None;
    let mut campaign = None;
    let mut actor = None;
    let mut kind = None;
    let mut reason = None;
    let mut evidence_ids = Vec::new();
    let mut positional = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--actor" => actor = Some(value()),
            "--reason" => reason = Some(value()),
            "--evidence" => evidence_ids.push(value()),
            "--kind" => {
                kind = Some(match value().as_str() {
                    "new-evidence" => review_core::ResolutionChallengeKind::NewEvidence,
                    "higher-severity" => review_core::ResolutionChallengeKind::HigherSeverity,
                    "outside-scope" => review_core::ResolutionChallengeKind::OutsideScope,
                    "expired" => review_core::ResolutionChallengeKind::Expired,
                    _ => usage(),
                })
            }
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let [finding_id] = positional.as_slice() else {
        usage()
    };
    ChallengeResolutionOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        finding_id: finding_id.clone(),
        kind: kind.unwrap_or_else(|| usage()),
        actor,
        reason: reason.unwrap_or_else(|| usage()),
        evidence_ids,
    }
}

fn parse_policy_time(mut args: std::env::Args) -> PolicyTimeOptions {
    let mut state = None;
    let mut campaign = None;
    let mut actor = None;
    let mut reason = None;
    let mut positional = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--actor" => actor = Some(value()),
            "--reason" => reason = Some(value()),
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let [tick] = positional.as_slice() else {
        usage()
    };
    PolicyTimeOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        tick: tick.parse().unwrap_or_else(|_| usage()),
        actor,
        reason: reason.unwrap_or_else(|| usage()),
    }
}

fn parse_group(mut args: std::env::Args) -> GroupOptions {
    let mut state = None;
    let mut campaign = None;
    let mut positional = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let (Some(campaign), [from, into]) = (campaign, positional.as_slice()) else {
        usage()
    };
    GroupOptions {
        state,
        campaign,
        from: from.clone(),
        into: into.clone(),
    }
}

fn parse_evidence_add(mut args: std::env::Args) -> EvidenceAddOptions {
    let mut state = None;
    let mut campaign = None;
    let mut actor = None;
    let mut positional = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--actor" => actor = Some(value()),
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let (Some(campaign), [demand_id, file]) = (campaign, positional.as_slice()) else {
        usage()
    };
    EvidenceAddOptions {
        state,
        campaign,
        demand_id: demand_id.clone(),
        file: PathBuf::from(file),
        actor,
    }
}

fn parse_evidence_satisfy(mut args: std::env::Args) -> EvidenceSatisfyOptions {
    let mut state = None;
    let mut campaign = None;
    let mut policy_revision = None;
    let mut reason = None;
    let mut actor = None;
    let mut admit_reuse = false;
    let mut positional = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--policy" => policy_revision = Some(value()),
            "--reason" => reason = Some(value()),
            "--actor" => actor = Some(value()),
            "--admit-reuse" if !admit_reuse => admit_reuse = true,
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let (Some(campaign), Some(policy_revision), Some(reason), [demand_id, evidence_id]) =
        (campaign, policy_revision, reason, positional.as_slice())
    else {
        usage()
    };
    EvidenceSatisfyOptions {
        state,
        campaign,
        demand_id: demand_id.clone(),
        evidence_id: evidence_id.clone(),
        policy_revision,
        reason,
        actor,
        admit_reuse,
    }
}

fn parse_demand_waive(mut args: std::env::Args) -> DemandWaiveOptions {
    let mut state = None;
    let mut campaign = None;
    let mut actor = None;
    let mut policy_revision = None;
    let mut reason = None;
    let mut demand_id = None;
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--actor" => actor = Some(value()),
            "--policy" => policy_revision = Some(value()),
            "--reason" => reason = Some(value()),
            other if !other.starts_with("--") && demand_id.is_none() => {
                demand_id = Some(other.to_string())
            }
            _ => usage(),
        }
    }
    DemandWaiveOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        demand_id: demand_id.unwrap_or_else(|| usage()),
        actor,
        policy_revision: policy_revision.unwrap_or_else(|| usage()),
        reason: reason.unwrap_or_else(|| usage()),
    }
}

fn main() {
    let mut args = std::env::args();
    args.next();
    let namespace = args.next();
    if matches!(namespace.as_deref(), Some("--version" | "-V")) {
        println!("af {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if namespace.as_deref() == Some("provider") {
        if args.next().as_deref() != Some("status") || args.next().is_some() {
            usage();
        }
        providers::print_status();
        return;
    }
    if namespace.as_deref() == Some("onboard") {
        match onboard::command(args) {
            Ok(()) => return,
            Err(error) => {
                eprintln!("af onboard: {error}");
                std::process::exit(1);
            }
        }
    }
    if namespace.as_deref() == Some("task") {
        let result = match args.next().as_deref() {
            Some("start") => {
                init_review_workers();
                task::start(task::parse(args).unwrap_or_else(|_| usage())).map(|verified| {
                    if !verified {
                        std::process::exit(3);
                    }
                })
            }
            Some("deliver") => {
                task::deliver(task::parse_delivery(args).unwrap_or_else(|_| usage()))
            }
            Some("list") => {
                task::list(task::parse_inspect(args, false).unwrap_or_else(|_| usage()))
            }
            Some("show") => task::show(task::parse_inspect(args, true).unwrap_or_else(|_| usage())),
            _ => usage(),
        };
        match result {
            Ok(()) => return,
            Err(error) => {
                eprintln!("af task: {error}");
                std::process::exit(1);
            }
        }
    }
    if namespace.as_deref() != Some("review") {
        usage();
    }
    let command = args.next();
    let result = match command.as_deref() {
        Some("run") => {
            init_review_workers();
            run(&parse_run(args)).map(exit_for_verdict)
        }
        Some("ledger") => print_ledger(&parse_ledger(args)),
        Some("show") => show(&parse_show(args)),
        Some("report") => print_report(&parse_report(args)),
        Some("resolve") => resolve(&parse_resolve(args)),
        Some("attest-change") => attest_change(&parse_attest_change(args)),
        Some("verify-fix") => verify_fix(&parse_verify_fix(args)),
        Some("challenge-resolution") => challenge_resolution(&parse_challenge_resolution(args)),
        Some("policy-time") => match args.next().as_deref() {
            Some("advance") => advance_policy_time(&parse_policy_time(args)),
            _ => usage(),
        },
        Some("group") => group(&parse_group(args), false),
        Some("ungroup") => group(&parse_group(args), true),
        Some("evidence") => match args.next().as_deref() {
            Some("add") => add_evidence(&parse_evidence_add(args)),
            Some("satisfy") => satisfy_evidence(&parse_evidence_satisfy(args)),
            _ => usage(),
        },
        Some("demand") => match args.next().as_deref() {
            Some("waive") => waive_demand(&parse_demand_waive(args)),
            _ => usage(),
        },
        Some("tui") => {
            init_review_workers();
            tui::launch(parse_run(args))
        }
        Some(flag) if flag.starts_with("--") => {
            init_review_workers();
            run(&parse_run(std::iter::once(flag.to_string()).chain(args))).map(exit_for_verdict)
        }
        _ => usage(),
    };
    if let Err(error) = result {
        eprintln!("af review: {error}");
        std::process::exit(1);
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
    eprintln!(
        "round {}; {} findings, {} open; {} required demands open/stale",
        ledger.round,
        findings.len(),
        findings.iter().filter(|f| f.status == Status::Open).count(),
        open_required_demands
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
    Ok(())
}

fn print_report(options: &ReportOptions) -> Result<(), String> {
    debug_assert_eq!(options.format, "md");
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

    println!("# Review campaign `{}`", options.campaign);
    println!();
    println!("- Runs recorded: {}", reports.len());
    println!("- Ledger round: {}", ledger.round);
    println!(
        "- Final verdict: {}",
        reports
            .last()
            .map(|event| report_verdict(event))
            .transpose()?
            .unwrap_or_else(|| "not recorded".to_string())
    );
    println!();
    println!("## Runs");
    println!();
    println!("| Run | Verdict | Tokens |");
    println!("| ---: | --- | ---: |");
    for (index, event) in reports.iter().enumerate() {
        println!(
            "| {} | {} | {} |",
            index + 1,
            report_verdict(event)?,
            event.payload["spent_tokens"]
                .as_u64()
                .map_or("-".to_string(), |tokens| tokens.to_string())
        );
    }

    println!();
    println!("## Demands");
    let demands = ledger.demand_views();
    if demands.is_empty() {
        println!();
        println!("None.");
    } else {
        for demand in demands {
            println!();
            println!(
                "- **[{:?}, {:?}] {}** (`{}`)",
                demand.requirement, demand.status, demand.claim, demand.demand_id
            );
            println!("  - Why: {}", markdown_line(&demand.why));
            println!(
                "  - Suggested method: {}",
                markdown_line(&demand.suggested_method)
            );
            println!("  - Source: {}", demand.source);
        }
    }

    if let Some(evidence) = latest_round_evidence(&events, &cas)?
        && evidence.ledger_was_not_produced()
        && !evidence.available_node_results.is_empty()
    {
        println!();
        println!("## Recorded, not gathered");
        println!();
        println!(
            "The latest Round did not produce a Ledger because {}. These admitted results remain evidence only; they are not Findings, a clean Ledger, or convergence input.",
            evidence.absence_reason()
        );
        for result in evidence.available_node_results {
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
            for finding in result.findings {
                println!(
                    "  - [{}] {} — {}",
                    finding["severity"].as_str().unwrap_or("unknown"),
                    markdown_line(finding["title"].as_str().unwrap_or("untitled finding")),
                    markdown_line(finding["body"].as_str().unwrap_or("(no body)"))
                );
            }
        }
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
        let heading = effective_severity.map_or_else(
            || "Recorded, not blocking this Subject".to_string(),
            |severity| title_case(&format!("{severity:?}").to_lowercase()),
        );
        println!("### {heading}");
        let matching: Vec<_> = ledger
            .finding_views()
            .into_iter()
            .filter(|finding| finding.convergence_severity == effective_severity)
            .collect();
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
                format!("{:?}", finding.severity).to_lowercase(),
                finding
                    .convergence_severity
                    .map(|severity| format!("{severity:?}").to_lowercase())
                    .unwrap_or_else(|| "-".to_string()),
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
                .map(|report| {
                    if report.report_id.is_empty() {
                        format!(
                            "{} round {} scope={} at {}:{} (legacy import)",
                            report.source,
                            report.round,
                            report.scope_label(),
                            report.file,
                            report.line.map_or("-".to_string(), |line| line.to_string())
                        )
                    } else {
                        format!(
                            "{} round {} scope={} at {}:{} `{}`",
                            report.source,
                            report.round,
                            report.scope_label(),
                            report.file,
                            report.line.map_or("-".to_string(), |line| line.to_string()),
                            report.report_id
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            println!("  - Reports: {evidence}");
            for transition in &finding.history {
                println!(
                    "  - Resolution/history, round {}: {:?} - {}",
                    transition.round,
                    transition.kind,
                    markdown_line(transition.note.as_deref().unwrap_or("(no note)"))
                );
            }
        }
    }
    Ok(())
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
            Ok(match report.verdict {
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
            })
        }
        _ => Err(format!("{} is not a run report", event.event_type)),
    }
}

fn markdown_line(value: &str) -> String {
    value.lines().collect::<Vec<_>>().join(" ")
}

fn title_case(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
        None => String::new(),
    }
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

    for node in options.provider_bindings.keys() {
        if !loaded.reviewers().contains_key(node) {
            return Err(format!("--provider names unknown reviewer node `{node}`"));
        }
        if !loaded.packages().contains_key(node) {
            return Err(format!(
                "node `{node}` is an inline command; --provider is only valid for packaged reviewers"
            ));
        }
    }
    for node in store
        .provider_operation_nodes(&run_id, authority.round_event_id())
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
            providers::operation_id_for(provider_id, node, reviewer, &authority)
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
    let replayed_spend = if options.provider_bindings.is_empty() {
        0
    } else {
        store
            .round_committed_tokens(&run_id, authority.round_event_id())
            .map_err(|error| error.to_string())?
    };
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
                state_dir: &state,
                run_id: &run_id,
                authority: &authority,
                cas: &cas,
                store: &mut store,
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

    let auth = (
        std::env::var("CLAUDE_CONFIG_DIR").ok(),
        std::env::var("USER").ok(),
        home.clone(),
    );
    let mut kernel = Kernel::from_loaded(&cas, &mut store, &run_id, snapshot, &loaded, authority)?
        .with_ledger_projection(ledger_projection)?
        .with_checks(loaded.checks().to_vec())
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
                let program = std::path::Path::new(&command.program)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                match program.as_str() {
                    "claude" => {
                        let user = auth.1.clone().ok_or_else(|| {
                            format!("node `{node}`: Claude subscription auth requires USER")
                        })?;
                        let mut adapter =
                            review_runner_claude::ClaudeAdapter::from_package(package, timeout)
                                .map_err(|error| format!("{node}: {error}"))?
                                .with_auth(
                                    admissions
                                        .get(node)
                                        .map(|provider| provider.auth_dir_string())
                                        .transpose()?
                                        .or_else(|| auth.0.clone()),
                                    user,
                                    auth.2.clone(),
                                );
                        if let Some(focus) = &focus {
                            adapter = adapter.with_focus(focus);
                        }
                        Box::new(adapter)
                    }
                    "codex" => {
                        let codex_home = match admissions.get(node) {
                            Some(provider) => provider.auth_dir_string()?,
                            None => resolve_codex_home(
                                &home,
                                std::env::var_os("CODEX_HOME").as_deref(),
                            )?,
                        };
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

    let verdict = kernel.publish_report(&report, *loaded.convergence())?;
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
    Ok(verdict)
}

fn exit_for_verdict(verdict: RunVerdict) {
    match verdict {
        RunVerdict::Pass => {}
        RunVerdict::Fail(_) => std::process::exit(3),
        RunVerdict::Incomplete { .. } => std::process::exit(4),
    }
}

#[cfg(test)]
mod option_tests {
    use std::ffi::OsStr;

    use super::{Options, latest_round_evidence, resolve_codex_home, validate_campaign_name};

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

    #[test]
    fn codex_runner_uses_the_ambient_auth_context() {
        assert_eq!(
            resolve_codex_home("/home/operator", Some(OsStr::new("/contexts/codex"))).unwrap(),
            "/contexts/codex"
        );
        assert_eq!(
            resolve_codex_home("/home/operator", None).unwrap(),
            "/home/operator/.codex"
        );
        assert_eq!(
            resolve_codex_home("/home/operator", Some(OsStr::new(""))).unwrap_err(),
            "CODEX_HOME is empty"
        );
    }

    #[test]
    fn campaign_names_cannot_redirect_state() {
        for invalid in ["", "../reviewers/architecture", "nested/name", "."] {
            assert!(validate_campaign_name(invalid).is_err());
        }
        for valid in ["heavy", "reviewctl-tui", "round_4", "v2.1-audit"] {
            assert!(validate_campaign_name(valid).is_ok());
        }
    }

    #[test]
    fn repository_state_is_always_refused() {
        let repository = tempfile::tempdir().unwrap();
        let options = Options {
            repo: repository.path().to_path_buf(),
            pipeline: ".af/pipelines/review.toml".into(),
            state: Some(repository.path().join(".af/state/architecture")),
            campaign: Some("architecture".to_string()),
            focus: None,
            authority: None,
            uncommitted: false,
            restart_round: false,
            timeout: None,
            git_timeout: None,
            provider_bindings: std::collections::BTreeMap::new(),
            provider_resumes: std::collections::BTreeMap::new(),
            json: false,
        };
        let error = options.resolved_state_dir().unwrap_err();
        assert!(error.contains("state must live under XDG state"));
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
