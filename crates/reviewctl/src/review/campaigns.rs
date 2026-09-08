//! `af review campaigns` and `gc`: enumerating Campaign state beneath a root, and reclaiming it.

use std::collections::BTreeSet;
use std::path::Path;

use review_core::EventType;
use review_store::{Cas, EventStore, LedgerProjection};

use crate::review::report::{
    FindingsSummaryView, ReportRoundView, findings_summary, findings_summary_line, human_duration,
    last_closed_summary, optional_number, optional_tokens, report_round_authority, report_rounds,
    wall_span_ms,
};
use crate::{
    CampaignsFormat, CampaignsOptions, GcOptions, campaign_id, legacy_campaign_state_matches,
    open_campaign_store_read_only, resolve_filesystem_path, validate_legacy_campaign_name,
    xdg_state_root,
};

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
pub(crate) fn print_gc(options: &GcOptions) -> Result<(), String> {
    let requested_root = match &options.state_root {
        Some(root) => root.clone(),
        None => xdg_state_root()?.join("af/review/campaigns"),
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

#[derive(serde::Serialize)]
struct CampaignListView {
    schema: &'static str,
    campaigns: Vec<CampaignView>,
    problems: Vec<CampaignProblemView>,
}

#[derive(serde::Serialize)]
pub(crate) struct CampaignProblemView {
    pub(crate) directory: String,
    pub(crate) reason: String,
}

pub(crate) struct CampaignEnumeration {
    pub(crate) campaigns: Vec<CampaignView>,
    pub(crate) problems: Vec<CampaignProblemView>,
}

#[derive(serde::Serialize)]
pub(crate) struct CampaignView {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) authority_snapshot_id: String,
    pub(crate) campaign_manifest_id: String,
    pub(crate) subject_kind: review_core::SubjectKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) base_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_subject_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_closed_round: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_closed_epoch: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verdict: Option<String>,
    pub(crate) rounds: Vec<ReportRoundView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) wall_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) findings: Option<FindingsSummaryView>,
    /// The state directory's name beneath the root, and — when asked for — what it holds.
    pub(crate) state_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state_bytes: Option<u64>,
    /// Newest write to the event store, as Unix milliseconds; what `gc` ages by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_activity_unix_ms: Option<u64>,
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

pub(crate) fn print_campaigns(options: &CampaignsOptions) -> Result<(), String> {
    let requested_root = match &options.state_root {
        Some(root) => root.clone(),
        None => xdg_state_root()?.join("af/review/campaigns"),
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

pub(crate) fn enumerate_campaigns(
    root: &Path,
    with_sizes: bool,
) -> Result<CampaignEnumeration, String> {
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
