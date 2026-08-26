//! Trusted campaign bootstrap and immutable Round input selection.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use review_config::Definition;
use review_config::lock::{Lockfile, Registry};
use review_core::{
    AuthorityFileV1, CampaignBudgetV1, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, CampaignReviewerV1, ChangeSetV1, EventType, ReviewerPackageV1,
    RoundInputSupersededPayloadV1, RoundStartedPayloadV1, SourceSnapshot, SubjectKind, SubjectV1,
    run_report_closes_round,
};
use review_pipeline::RoundAuthority;
use review_runner::{MAX_CHANGE_SET_BYTES, MAX_PRIOR_FINDINGS_BYTES};
use review_source_git::{Capture, EntryKind, Manifest, Repo, Snapshot};
use review_store::{Cas, EventStore, Ingest, Ledger, LedgerProjection, NewEvent, Status};

use crate::{Options, campaign_run_id};

pub(super) fn requested_git_timeout(configured: Option<Duration>) -> Duration {
    configured.unwrap_or(Duration::from_secs(
        review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS,
    ))
}

pub(super) struct PreparedRun {
    pub loaded: review_config::Loaded,
    pub snapshot: Manifest,
    pub run_id: String,
    pub focus: Option<String>,
    pub timeout: Duration,
    pub check_timeout: Duration,
    pub git_timeout: Duration,
    pub authority: RoundAuthority,
    pub ledger_projection: LedgerProjection,
}

struct OpenCampaign {
    loaded: review_config::Loaded,
    manifest: CampaignManifestV1,
    manifest_id: String,
    opened_event_id: String,
}

struct RoundInput {
    payload: RoundStartedPayloadV1,
    event_id: String,
    snapshot: Manifest,
    prior_count: usize,
    ledger_projection: LedgerProjection,
}

pub(super) fn prepare(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
) -> Result<PreparedRun, String> {
    let run_id = options
        .campaign
        .as_deref()
        .map(campaign_run_id)
        .unwrap_or_else(|| campaign_run_id("local"));
    let pipeline_path = authority_path(&options.repo, &options.pipeline)?;
    let events = store.replay(&run_id).map_err(|error| error.to_string())?;
    let (campaign, events) = if events.is_empty() {
        let campaign = open_new(options, cas, store, repo, &run_id, &pipeline_path)?;
        let events = store.replay(&run_id).map_err(|error| error.to_string())?;
        (campaign, events)
    } else {
        let campaign = resume(options, cas, &events, &pipeline_path)?;
        (campaign, events)
    };

    let round = prepare_round(options, cas, store, repo, &run_id, &campaign, &events)?;
    let authority = RoundAuthority::load(store, cas, &run_id, &round.event_id)?;
    let check_timeout_seconds = campaign
        .manifest
        .check_timeout_seconds
        .unwrap_or(campaign.loaded.check_timeout_seconds());
    let git_timeout_seconds = campaign
        .manifest
        .git_timeout_seconds
        .unwrap_or(review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS);
    Ok(PreparedRun {
        loaded: campaign.loaded,
        snapshot: round.snapshot,
        run_id,
        focus: campaign.manifest.focus,
        timeout: Duration::from_secs(campaign.manifest.reviewer_timeout_seconds),
        check_timeout: Duration::from_secs(check_timeout_seconds),
        git_timeout: Duration::from_secs(git_timeout_seconds),
        authority,
        ledger_projection: round.ledger_projection,
    })
}

fn open_new(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
    run_id: &str,
    pipeline_path: &str,
) -> Result<OpenCampaign, String> {
    let authority_ref = options.authority.as_deref().ok_or(
        "a new Campaign requires trusted invocation policy `--authority REV`; continuation \
         reuses the stored authority and does not resolve the ref again",
    )?;
    let snapshot = Capture::new(repo, cas)
        .committed(authority_ref)
        .map_err(|error| format!("capturing authority `{authority_ref}`: {error}"))?;
    let (authority_snapshot_id, authority_manifest_id) = publish_snapshot(&snapshot, cas)?;

    let layout = authority_layout(pipeline_path)?;
    let lock_path = layout.lock.clone();
    let pipeline_bytes = authority_bytes(&snapshot.manifest, cas, pipeline_path)?;
    let lock_bytes = authority_bytes(&snapshot.manifest, cas, &lock_path)?;
    let pipeline_text = std::str::from_utf8(&pipeline_bytes)
        .map_err(|error| format!("authority pipeline `{pipeline_path}` is not UTF-8: {error}"))?;
    let lock_text = std::str::from_utf8(&lock_bytes)
        .map_err(|error| format!("authority lock `{lock_path}` is not UTF-8: {error}"))?;
    let lockfile = Lockfile::from_toml(lock_text).map_err(|error| error.to_string())?;
    if layout.root == ".af" {
        validate_af_pipeline_pin(&lockfile, pipeline_path, &pipeline_bytes)?;
    }
    let registry = Registry::captured(captured_registry(
        &snapshot.manifest,
        cas,
        &layout.registry,
    )?);
    let loaded = Definition::from_toml(pipeline_text)
        .map_err(|error| error.to_string())?
        .load_with(&lockfile, &registry)
        .map_err(|error| error.to_string())?;

    let pipeline_artifact_id = cas
        .put(&pipeline_bytes)
        .map_err(|error| error.to_string())?;
    let lock_artifact_id = cas.put(&lock_bytes).map_err(|error| error.to_string())?;
    let project_policy_ids = if layout.root == ".af" {
        let project_path = ".af/af.toml";
        let project_bytes = authority_bytes(&snapshot.manifest, cas, project_path)?;
        validate_af_project(&project_bytes, pipeline_path)?;
        vec![cas.put(&project_bytes).map_err(|error| error.to_string())?]
    } else {
        Vec::new()
    };
    let finding_genesis_id = cas
        .put_json(&serde_json::json!({
            "kind": "finding-set-genesis@1",
            "authority_snapshot_id": authority_snapshot_id,
            "findings": [],
        }))
        .map_err(|error| error.to_string())?;
    let demand_genesis_id = cas
        .put_json(&serde_json::json!({
            "kind": "demand-set-genesis@1",
            "authority_snapshot_id": authority_snapshot_id,
            "demands": [],
        }))
        .map_err(|error| error.to_string())?;

    let mut package_artifacts: BTreeMap<(String, String), (String, Vec<String>)> = BTreeMap::new();
    let mut reviewers = Vec::new();
    for (node, package) in loaded.packages() {
        let key = (package.name.clone(), package.digest.clone());
        let (package_artifact_id, _) = match package_artifacts.get(&key) {
            Some(existing) => existing.clone(),
            None => {
                let mut files = BTreeMap::new();
                let mut file_ids = Vec::new();
                for (path, bytes) in package.files() {
                    let artifact_id = cas.put(bytes).map_err(|error| error.to_string())?;
                    files.insert(path.clone(), artifact_id.clone());
                    file_ids.push(artifact_id);
                }
                let artifact = ReviewerPackageV1 {
                    name: package.name.clone(),
                    version: package.version.clone(),
                    digest: package.digest.clone(),
                    files,
                };
                artifact.validate()?;
                let artifact_id = cas
                    .put_json(&serde_json::to_value(&artifact).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                package_artifacts.insert(key.clone(), (artifact_id.clone(), file_ids.clone()));
                (artifact_id, file_ids)
            }
        };
        reviewers.push(CampaignReviewerV1 {
            node: node.clone(),
            name: package.name.clone(),
            version: package.version.clone(),
            digest: package.digest.clone(),
            package_artifact_id,
        });
    }

    let mut execution_policy_ids = BTreeSet::from([pipeline_artifact_id.clone()]);
    execution_policy_ids.extend(
        reviewers
            .iter()
            .map(|reviewer| reviewer.package_artifact_id.clone()),
    );
    let convergence = loaded.convergence();
    let budgets = loaded.budgets().map(|budget| CampaignBudgetV1 {
        attempt_tokens: budget.attempt,
        run_tokens: budget.run,
    });
    let manifest = CampaignManifestV1 {
        authority_snapshot_id: authority_snapshot_id.clone(),
        subject_kind: loaded.subject_kind(),
        base_snapshot_id: (loaded.subject_kind() == SubjectKind::Diff)
            .then(|| authority_snapshot_id.clone()),
        pipeline: AuthorityFileV1 {
            path: pipeline_path.to_string(),
            artifact_id: pipeline_artifact_id.clone(),
        },
        reviewer_lock: AuthorityFileV1 {
            path: lock_path,
            artifact_id: lock_artifact_id.clone(),
        },
        reviewers,
        execution_policy_ids: execution_policy_ids.into_iter().collect(),
        project_policy_ids: project_policy_ids.clone(),
        convergence: CampaignConvergenceV1 {
            clean_rounds: convergence.clean_rounds,
            max_rounds: convergence.max_rounds,
            gate: format!("{:?}", convergence.gate).to_lowercase(),
        },
        reviewer_timeout_seconds: options
            .timeout
            .unwrap_or(Duration::from_secs(1800))
            .as_secs(),
        check_timeout_seconds: Some(loaded.check_timeout_seconds()),
        git_timeout_seconds: Some(requested_git_timeout(options.git_timeout).as_secs()),
        budgets,
        focus: options.focus.clone(),
        finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string(),
        finding_genesis_id,
        demand_genesis_id,
    };
    manifest.validate()?;
    let manifest_id = cas
        .put_json(&serde_json::to_value(&manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let mut refs = vec![
        authority_snapshot_id.clone(),
        authority_manifest_id,
        manifest_id.clone(),
        pipeline_artifact_id,
        lock_artifact_id,
        manifest.finding_genesis_id.clone(),
        manifest.demand_genesis_id.clone(),
    ];
    refs.extend(project_policy_ids);
    for (package_id, file_ids) in package_artifacts.values() {
        refs.push(package_id.clone());
        refs.extend(file_ids.iter().cloned());
    }
    refs.sort();
    refs.dedup();
    let payload = CampaignOpenedPayloadV1 {
        campaign_manifest_id: manifest_id.clone(),
        authority_snapshot_id: authority_snapshot_id.clone(),
    };
    let opened = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                serde_json::to_value(payload).map_err(|error| error.to_string())?,
            )
            .correlating(manifest_id.clone())
            .referencing(refs),
        )
        .map_err(|error| error.to_string())?;
    super::run_progress(options, format_args!("authority {authority_snapshot_id}"));
    super::run_progress(options, format_args!("manifest  {manifest_id}"));
    Ok(OpenCampaign {
        loaded,
        manifest,
        manifest_id,
        opened_event_id: opened.event_id,
    })
}

fn resume(
    options: &Options,
    cas: &Cas,
    events: &[review_core::RunEvent],
    pipeline_path: &str,
) -> Result<OpenCampaign, String> {
    let mut opened = events
        .iter()
        .filter(|event| event.event_type == EventType::CampaignOpenedV1);
    let event = opened.next().ok_or(
        "campaign state predates CampaignOpened@1; start a new Campaign rather than inventing authority",
    )?;
    if opened.next().is_some() {
        return Err("campaign contains more than one CampaignOpened@1 event".into());
    }
    let payload: CampaignOpenedPayloadV1 =
        serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
    payload.validate()?;
    let manifest: CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&payload.campaign_manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    manifest.validate()?;
    if manifest.authority_snapshot_id != payload.authority_snapshot_id {
        return Err("CampaignOpened@1 disagrees with its CampaignManifest authority".into());
    }
    if manifest.pipeline.path != pipeline_path {
        return Err(format!(
            "campaign is pinned to pipeline `{}`; `{pipeline_path}` requires a new Campaign",
            manifest.pipeline.path
        ));
    }
    if options
        .focus
        .as_ref()
        .is_some_and(|focus| Some(focus) != manifest.focus.as_ref())
    {
        return Err("invocation focus differs from the pinned Campaign manifest".into());
    }
    if options
        .timeout
        .is_some_and(|timeout| timeout.as_secs() != manifest.reviewer_timeout_seconds)
    {
        return Err("reviewer timeout differs from the pinned Campaign manifest".into());
    }
    let pinned_git_timeout = manifest
        .git_timeout_seconds
        .unwrap_or(review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS);
    if requested_git_timeout(options.git_timeout).as_secs() != pinned_git_timeout {
        return Err("Git capture timeout differs from the pinned Campaign manifest".into());
    }

    let pipeline = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|error| error.to_string())?;
    let lock = cas
        .get(&manifest.reviewer_lock.artifact_id)
        .map_err(|error| error.to_string())?;
    let pipeline = std::str::from_utf8(&pipeline).map_err(|error| error.to_string())?;
    let lockfile =
        Lockfile::from_toml(std::str::from_utf8(&lock).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let mut packages: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    let mut captured: BTreeMap<String, (ReviewerPackageV1, BTreeMap<String, Vec<u8>>)> =
        BTreeMap::new();
    for binding in &manifest.reviewers {
        if !captured.contains_key(&binding.package_artifact_id) {
            let package: ReviewerPackageV1 = serde_json::from_value(
                cas.get_json(&binding.package_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            package.validate()?;
            let mut files = BTreeMap::new();
            for (path, artifact_id) in &package.files {
                files.insert(
                    path.clone(),
                    cas.get(artifact_id).map_err(|error| error.to_string())?,
                );
            }
            let recomputed = review_config::lock::package_digest_from_files(&files);
            if recomputed != package.digest {
                return Err(format!(
                    "captured reviewer package `{}` claims digest {} but contains {recomputed}",
                    package.name, package.digest
                ));
            }
            captured.insert(binding.package_artifact_id.clone(), (package, files));
        }
        let (package, files) = captured
            .get(&binding.package_artifact_id)
            .expect("captured package inserted");
        if package.name != binding.name
            || package.version != binding.version
            || package.digest != binding.digest
        {
            return Err(format!(
                "captured reviewer package for node `{}` disagrees with CampaignManifest@1",
                binding.node
            ));
        }
        if packages
            .insert(package.name.clone(), files.clone())
            .is_some_and(|prior| &prior != files)
        {
            return Err(format!(
                "CampaignManifest@1 binds package `{}` to inconsistent bytes",
                package.name
            ));
        }
    }
    let registry = Registry::captured(packages);
    let loaded = Definition::from_toml(pipeline)
        .map_err(|error| error.to_string())?
        .load_with(&lockfile, &registry)
        .map_err(|error| error.to_string())?;
    if loaded.subject_kind() != manifest.subject_kind {
        return Err("captured pipeline disagrees with CampaignManifest Subject kind".into());
    }
    validate_manifest_authority(cas, &manifest, &loaded, &captured)?;
    super::run_progress(
        options,
        format_args!("authority {} (pinned)", manifest.authority_snapshot_id),
    );
    super::run_progress(
        options,
        format_args!("manifest  {} (resumed)", payload.campaign_manifest_id),
    );
    Ok(OpenCampaign {
        loaded,
        manifest,
        manifest_id: payload.campaign_manifest_id,
        opened_event_id: event.event_id.clone(),
    })
}

fn validate_manifest_authority(
    cas: &Cas,
    manifest: &CampaignManifestV1,
    loaded: &review_config::Loaded,
    captured: &BTreeMap<String, (ReviewerPackageV1, BTreeMap<String, Vec<u8>>)>,
) -> Result<(), String> {
    let convergence = loaded.convergence();
    if manifest.convergence.clean_rounds != convergence.clean_rounds
        || manifest.convergence.max_rounds != convergence.max_rounds
        || manifest.convergence.gate != format!("{:?}", convergence.gate).to_lowercase()
    {
        return Err("CampaignManifest convergence differs from captured pipeline authority".into());
    }
    let budgets = loaded.budgets().map(|budget| CampaignBudgetV1 {
        attempt_tokens: budget.attempt,
        run_tokens: budget.run,
    });
    if manifest.budgets != budgets {
        return Err("CampaignManifest budgets differ from captured pipeline authority".into());
    }
    if manifest
        .check_timeout_seconds
        .unwrap_or(loaded.check_timeout_seconds())
        != loaded.check_timeout_seconds()
    {
        return Err(
            "CampaignManifest check timeout differs from captured pipeline authority".into(),
        );
    }
    if manifest.reviewers.len() != loaded.packages().len() {
        return Err("CampaignManifest reviewer bindings are incomplete".into());
    }
    for (node, package) in loaded.packages() {
        let binding = manifest
            .reviewers
            .iter()
            .find(|binding| binding.node == *node)
            .ok_or_else(|| format!("CampaignManifest has no reviewer binding for `{node}`"))?;
        if binding.name != package.name
            || binding.version != package.version
            || binding.digest != package.digest
        {
            return Err(format!(
                "CampaignManifest reviewer binding for `{node}` differs from resolved authority"
            ));
        }
    }
    let expected_execution: BTreeSet<String> =
        std::iter::once(manifest.pipeline.artifact_id.clone())
            .chain(
                manifest
                    .reviewers
                    .iter()
                    .map(|binding| binding.package_artifact_id.clone()),
            )
            .collect();
    if manifest
        .execution_policy_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected_execution
    {
        return Err("CampaignManifest execution policy IDs are not the resolved authority".into());
    }
    for policy in &manifest.project_policy_ids {
        cas.get(policy).map_err(|error| error.to_string())?;
    }
    for (id, kind) in [
        (&manifest.finding_genesis_id, "finding-set-genesis@1"),
        (&manifest.demand_genesis_id, "demand-set-genesis@1"),
    ] {
        let root = cas.get_json(id).map_err(|error| error.to_string())?;
        if root["kind"] != kind || root["authority_snapshot_id"] != manifest.authority_snapshot_id {
            return Err(format!("CampaignManifest has an invalid `{kind}` root"));
        }
    }

    let authority: SourceSnapshot = serde_json::from_value(
        cas.get_json(&manifest.authority_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let authority_manifest_id = authority
        .artifact_manifest
        .ok_or("Authority Snapshot has no artifact manifest")?;
    let tree: Manifest = serde_json::from_value(
        cas.get_json(&authority_manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if tree.content_digest() != authority.content_digest
        || tree
            .get(&manifest.pipeline.path)
            .map(|entry| &entry.content)
            != Some(&manifest.pipeline.artifact_id)
        || tree
            .get(&manifest.reviewer_lock.path)
            .map(|entry| &entry.content)
            != Some(&manifest.reviewer_lock.artifact_id)
    {
        return Err("CampaignManifest authority files are not reachable from its Snapshot".into());
    }
    let layout = authority_layout(&manifest.pipeline.path)?;
    for (package, _) in captured.values() {
        for (path, artifact_id) in &package.files {
            let authority_path = format!("{}/{}/{path}", layout.registry, package.name);
            if tree.get(&authority_path).map(|entry| &entry.content) != Some(artifact_id) {
                return Err(format!(
                    "captured reviewer file `{authority_path}` is not authority Snapshot content"
                ));
            }
        }
    }
    Ok(())
}

fn prepare_round(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
    run_id: &str,
    campaign: &OpenCampaign,
    events: &[review_core::RunEvent],
) -> Result<RoundInput, String> {
    let authority_snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&campaign.manifest.authority_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let repository_id = authority_snapshot.repository_id.clone();
    let ledger_projection =
        LedgerProjection::from_events(run_id, events, cas).map_err(|error| error.to_string())?;
    let mut closed_rounds = 0_u32;
    for event in events {
        if run_report_closes_round(event)
            .map_err(|error| format!("decoding {}: {error}", event.event_type))?
            .unwrap_or(false)
        {
            closed_rounds += 1;
        }
    }
    let target_round = closed_rounds + 1;
    let mut starts: Vec<(&review_core::RunEvent, RoundStartedPayloadV1)> = events
        .iter()
        .filter(|event| event.event_type == EventType::RoundStartedV1)
        .filter_map(|event| {
            serde_json::from_value::<RoundStartedPayloadV1>(event.payload.clone())
                .ok()
                .filter(|payload| payload.round == target_round)
                .map(|payload| (event, payload))
        })
        .collect();
    starts.sort_by_key(|(_, payload)| payload.epoch);
    let existing = starts.last().cloned();

    let round = match (existing, options.restart_round) {
        (Some((event, payload)), false) => load_round(
            options,
            cas,
            event.event_id.clone(),
            payload,
            &repository_id,
            ledger_projection,
        )?,
        (None, true) => {
            return Err("--restart-round requires an incomplete Round to supersede".into());
        }
        (existing, _restart) => capture_round(
            options,
            cas,
            store,
            repo,
            run_id,
            campaign,
            RoundCaptureRequest {
                round: target_round,
                superseded: existing.as_ref().map(|(event, payload)| (*event, payload)),
                ledger_projection,
            },
        )?,
    };

    let mut round = round;
    {
        let mut ingest =
            Ingest::from_projection(store, cas, run_id.to_string(), round.ledger_projection)
                .map_err(|error| error.to_string())?
                .under_round(&round.event_id);
        while ingest.ledger().round < target_round {
            ingest.advance().map_err(|error| error.to_string())?;
        }
        round.ledger_projection = ingest.into_projection();
    }
    super::run_progress(
        options,
        format_args!(
            "round    {} (epoch {})",
            round.payload.round, round.payload.epoch
        ),
    );
    if round.prior_count > 0 {
        super::run_progress(
            options,
            format_args!("prior    {} findings carried", round.prior_count),
        );
    }
    Ok(round)
}

struct RoundCaptureRequest<'a> {
    round: u32,
    superseded: Option<(&'a review_core::RunEvent, &'a RoundStartedPayloadV1)>,
    ledger_projection: LedgerProjection,
}

fn capture_round(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
    run_id: &str,
    campaign: &OpenCampaign,
    request: RoundCaptureRequest<'_>,
) -> Result<RoundInput, String> {
    let RoundCaptureRequest {
        round,
        superseded,
        mut ledger_projection,
    } = request;
    let dispatched_attempts: Vec<(String, String, Option<u64>)> = if let Some((old_event, _)) =
        superseded
    {
        let events = store.replay(run_id).map_err(|error| error.to_string())?;
        if events.iter().any(|event| {
            event.sequence > old_event.sequence
                && (event.event_type == EventType::FindingReportedV1
                    || (event.event_type == EventType::FindingResolvedV1
                        && event.causation_id.as_deref() == Some(old_event.event_id.as_str())))
        }) {
            return Err(
                "cannot supersede an incomplete Round after it published finding state; start a new Campaign"
                    .into(),
            );
        }
        let mut live = BTreeMap::new();
        for event in events
            .into_iter()
            .filter(|event| event.causation_id.as_deref() == Some(old_event.event_id.as_str()))
        {
            let Some(attempt) = event.attempt_id.clone() else {
                continue;
            };
            match event.event_type {
                EventType::AttemptDispatchedV1 => {
                    live.insert(
                        attempt,
                        (
                            event.node_id.ok_or("AttemptDispatched@1 has no node ID")?,
                            event.payload["reserved"].as_u64(),
                        ),
                    );
                }
                EventType::AttemptAdmittedV1
                | EventType::AttemptFailedV1
                | EventType::AttemptFencedV1
                | EventType::AttemptReleasedV1 => {
                    live.remove(&attempt);
                }
                _ => {}
            }
        }
        live.into_iter()
            .map(|(attempt, (node, charged))| (node, attempt, charged))
            .collect()
    } else {
        Vec::new()
    };
    let capture = Capture::new(repo, cas);
    let mut snapshot = if options.uncommitted {
        capture
            .dirty()
            .map_err(|error| format!("capturing revalidated worktree: {error}"))?
    } else {
        capture
            .committed("HEAD")
            .map_err(|error| format!("capturing HEAD: {error}"))?
    };
    let authority_snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&campaign.manifest.authority_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if snapshot.repository_id != authority_snapshot.repository_id {
        return Err(
            "candidate HEAD belongs to a different repository than the Campaign authority".into(),
        );
    }
    if campaign.loaded.subject_kind() == SubjectKind::Diff
        && snapshot.submodules != authority_snapshot.submodules
    {
        let mut paths: Vec<String> = snapshot
            .submodules
            .iter()
            .chain(&authority_snapshot.submodules)
            .map(|submodule| submodule.path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        return Err(format!(
            "diff capture refuses changed gitlinks until submodule sandbox policy is explicit: {}",
            paths.join(", ")
        ));
    }
    let tree_diff = if campaign.loaded.subject_kind() == SubjectKind::Diff {
        let base_manifest_id = authority_snapshot
            .artifact_manifest
            .as_deref()
            .ok_or("Campaign Base Snapshot has no artifact manifest")?;
        let base_manifest: Manifest = serde_json::from_value(
            cas.get_json(base_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let base_tree = capture
            .rehydrate_committed(&authority_snapshot, &base_manifest)
            .map_err(|error| format!("rehydrating pinned Base: {error}"))?;
        if snapshot.dirty {
            let (head_tree, diff) = repo
                .tree_diff_synthetic_head(&base_tree, &snapshot.manifest, cas)
                .map_err(|error| error.to_string())?;
            snapshot.tree_id = Some(head_tree);
            Some(diff)
        } else {
            Some(
                repo.tree_diff(
                    &base_tree,
                    snapshot
                        .tree_id
                        .as_ref()
                        .ok_or("committed head has no tree authority")?,
                )
                .map_err(|error| error.to_string())?,
            )
        }
    } else {
        if snapshot.dirty {
            snapshot.tree_id = Some(
                repo.synthetic_tree(&snapshot.manifest, cas)
                    .map_err(|error| error.to_string())?,
            );
        }
        None
    };
    let (head_snapshot_id, manifest_id) = publish_snapshot(&snapshot, cas)?;
    let change_set_id = match tree_diff {
        Some(diff) => {
            // Base64 alone expands every three raw bytes to four encoded bytes. Refuse before
            // building path arrays, base64, serde Values, and canonical JSON when the patch
            // already cannot fit the authoritative encoded Change Set bound below.
            let raw_patch_limit = maximum_raw_patch_bytes();
            if raw_patch_exceeds_change_set_bound(diff.patch().len()) {
                return Err(format!(
                    "exact Change Set patch is {} raw bytes; maximum encodable patch is {} raw bytes and partitioning is required",
                    diff.patch().len(),
                    raw_patch_limit
                ));
            }
            let base_snapshot_id = campaign
                .manifest
                .base_snapshot_id
                .as_deref()
                .ok_or("diff Campaign has no pinned Base Snapshot")?;
            let change_set = diff.change_set(base_snapshot_id, &head_snapshot_id)?;
            let value = serde_json::to_value(&change_set).map_err(|error| error.to_string())?;
            review_core::json::admit(&value).map_err(|error| error.to_string())?;
            let encoded =
                review_store::canonical::canonicalize(&value).map_err(|error| error.to_string())?;
            if encoded.len() > MAX_CHANGE_SET_BYTES {
                return Err(format!(
                    "exact Change Set is {} bytes; maximum is {} bytes and partitioning is required",
                    encoded.len(),
                    MAX_CHANGE_SET_BYTES
                ));
            }
            Some(cas.put(&encoded).map_err(|error| error.to_string())?)
        }
        None => None,
    };
    let mut source_refs = vec![
        campaign.manifest.authority_snapshot_id.clone(),
        campaign.manifest_id.clone(),
        head_snapshot_id.clone(),
        manifest_id.clone(),
    ];
    source_refs.extend(change_set_id.clone());
    let source_captured = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::SourceCapturedV1,
                snapshot
                    .to_payload(Some(&manifest_id))
                    .map_err(|error| error.to_string())?,
            )
            .caused_by(campaign.opened_event_id.clone())
            .correlating(head_snapshot_id.clone())
            .referencing(source_refs),
        )
        .map_err(|error| error.to_string())?;
    ledger_projection
        .apply_event(&source_captured, cas)
        .map_err(|error| error.to_string())?;
    let subject = match campaign.loaded.subject_kind() {
        SubjectKind::WholeTree => SubjectV1::whole_tree(&head_snapshot_id),
        SubjectKind::Diff => SubjectV1::diff(
            &head_snapshot_id,
            campaign
                .manifest
                .base_snapshot_id
                .as_deref()
                .ok_or("diff Campaign has no pinned Base Snapshot")?,
            change_set_id
                .as_deref()
                .ok_or("diff Campaign produced no Change Set")?,
        ),
    };
    subject.validate()?;
    let subject_id = cas
        .put_json(&serde_json::to_value(&subject).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;

    let (prior_findings, demands) = if let Some((_, old)) = superseded {
        let findings = serde_json::Value::Array(prior_rows(ledger_projection.ledger()));
        let demands = cas
            .get_json(&old.prior_demand_set_id)
            .map_err(|error| error.to_string())?["demands"]
            .clone();
        (findings, demands)
    } else {
        (
            serde_json::Value::Array(prior_rows(ledger_projection.ledger())),
            serde_json::Value::Array(Vec::new()),
        )
    };
    let prior_count = prior_findings.as_array().map_or(0, Vec::len);
    let prior_finding_set = serde_json::json!({
        "subject_id": subject_id,
        "round": round,
        "prior_findings": prior_findings,
    });
    let prior_bytes = serde_json::to_string_pretty(&prior_finding_set)
        .map_err(|error| error.to_string())?
        .len();
    if prior_bytes > MAX_PRIOR_FINDINGS_BYTES {
        return Err(format!(
            "exact prior Finding Set is {prior_bytes} bytes; maximum is {MAX_PRIOR_FINDINGS_BYTES} bytes and partitioning is required"
        ));
    }
    let prior_finding_set_id = cas
        .put_json(&prior_finding_set)
        .map_err(|error| error.to_string())?;
    let prior_demand_set_id = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id,
            "round": round,
            "demands": demands,
        }))
        .map_err(|error| error.to_string())?;
    let payload = RoundStartedPayloadV1 {
        round,
        epoch: superseded.map_or(1, |(_, old)| old.epoch + 1),
        campaign_manifest_id: campaign.manifest_id.clone(),
        subject_id: subject_id.clone(),
        prior_finding_set_id,
        prior_demand_set_id,
    };
    payload.validate()?;
    let started = if let Some((old_event, old)) = superseded {
        let superseded = RoundInputSupersededPayloadV1 {
            round,
            old_epoch: old.epoch,
            new_epoch: payload.epoch,
            campaign_manifest_id: campaign.manifest_id.clone(),
            old_subject_id: old.subject_id.clone(),
            replacement_subject_id: payload.subject_id.clone(),
        };
        superseded.validate()?;
        let mut replacement_refs = vec![
            campaign.manifest.authority_snapshot_id.clone(),
            campaign.manifest_id.clone(),
            old.subject_id.clone(),
            payload.subject_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        replacement_refs.extend(subject.base_snapshot_id.clone());
        replacement_refs.extend(subject.change_set_id.clone());
        let mut batch = vec![
            NewEvent::new(
                EventType::RoundInputSupersededV1,
                serde_json::to_value(superseded).map_err(|error| error.to_string())?,
            )
            .caused_by(old_event.event_id.clone())
            .correlating(payload.subject_id.clone())
            .referencing(replacement_refs),
        ];
        batch.extend(
            dispatched_attempts
                .into_iter()
                .map(|(node, attempt, charged)| {
                    NewEvent::new(
                        EventType::AttemptFencedV1,
                        serde_json::json!({
                            "reason": "Round input superseded",
                            "charged": charged,
                        }),
                    )
                    .node(node)
                    .attempt(attempt)
                    .caused_by(old_event.event_id.clone())
                }),
        );
        let mut round_refs = vec![
            campaign.manifest.authority_snapshot_id.clone(),
            campaign.manifest_id.clone(),
            head_snapshot_id.clone(),
            payload.subject_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        round_refs.extend(subject.base_snapshot_id.clone());
        round_refs.extend(subject.change_set_id.clone());
        batch.push(
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(&payload).map_err(|error| error.to_string())?,
            )
            .caused_by(old_event.event_id.clone())
            .correlating(subject_id.clone())
            .referencing(round_refs),
        );
        let appended = store
            .append_batch(run_id, cas, &batch)
            .map_err(|error| error.to_string())?;
        for event in &appended {
            ledger_projection
                .apply_event(event, cas)
                .map_err(|error| error.to_string())?;
        }
        appended
            .last()
            .cloned()
            .ok_or("supersession batch did not publish its replacement Round")?
    } else {
        let mut round_refs = vec![
            campaign.manifest.authority_snapshot_id.clone(),
            campaign.manifest_id.clone(),
            head_snapshot_id.clone(),
            payload.subject_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        round_refs.extend(subject.base_snapshot_id.clone());
        round_refs.extend(subject.change_set_id.clone());
        let started = store
            .append(
                run_id,
                cas,
                NewEvent::new(
                    EventType::RoundStartedV1,
                    serde_json::to_value(&payload).map_err(|error| error.to_string())?,
                )
                .caused_by(campaign.opened_event_id.clone())
                .correlating(subject_id)
                .referencing(round_refs),
            )
            .map_err(|error| error.to_string())?;
        ledger_projection
            .apply_event(&started, cas)
            .map_err(|error| error.to_string())?;
        started
    };
    super::run_progress(
        options,
        format_args!("snapshot {}", snapshot.content_digest),
    );
    Ok(RoundInput {
        payload,
        event_id: started.event_id,
        snapshot: snapshot.manifest,
        prior_count,
        ledger_projection,
    })
}

fn maximum_raw_patch_bytes() -> usize {
    MAX_CHANGE_SET_BYTES.saturating_mul(3) / 4
}

fn raw_patch_exceeds_change_set_bound(raw_bytes: usize) -> bool {
    raw_bytes > maximum_raw_patch_bytes()
}

fn load_round(
    options: &Options,
    cas: &Cas,
    event_id: String,
    payload: RoundStartedPayloadV1,
    repository_id: &str,
    ledger_projection: LedgerProjection,
) -> Result<RoundInput, String> {
    payload.validate()?;
    let subject: SubjectV1 = serde_json::from_value(
        cas.get_json(&payload.subject_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    subject.validate()?;
    if subject.kind == SubjectKind::Diff {
        let change_set: ChangeSetV1 = serde_json::from_value(
            cas.get_json(
                subject
                    .change_set_id
                    .as_deref()
                    .ok_or("diff Subject has no Change Set ID")?,
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        change_set.validate()?;
        if change_set.base_snapshot_id != subject.base_snapshot_id.as_deref().unwrap_or_default()
            || change_set.head_snapshot_id != subject.head_snapshot_id
        {
            return Err("ChangeSet@1 does not match the resumed Subject".into());
        }
    }
    let snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&subject.head_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if snapshot.repository_id != repository_id {
        return Err("captured Round Subject belongs to a different repository".into());
    }
    let manifest_id = snapshot
        .artifact_manifest
        .ok_or("captured head Snapshot has no artifact manifest")?;
    let manifest: Manifest = serde_json::from_value(
        cas.get_json(&manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if manifest.content_digest() != snapshot.content_digest {
        return Err("captured head manifest disagrees with SourceSnapshot content digest".into());
    }
    let prior_count = validate_round_set(
        cas,
        &payload.prior_finding_set_id,
        &payload.subject_id,
        payload.round,
        "prior_findings",
    )?;
    validate_round_set(
        cas,
        &payload.prior_demand_set_id,
        &payload.subject_id,
        payload.round,
        "demands",
    )?;
    super::run_progress(
        options,
        format_args!("snapshot {} (reused)", snapshot.content_digest),
    );
    Ok(RoundInput {
        payload,
        event_id,
        snapshot: manifest,
        prior_count,
        ledger_projection,
    })
}

fn validate_round_set(
    cas: &Cas,
    artifact_id: &str,
    subject_id: &str,
    round: u32,
    items_field: &str,
) -> Result<usize, String> {
    let value = cas
        .get_json(artifact_id)
        .map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("Round {items_field} set is not an object"))?;
    let expected = BTreeSet::from(["subject_id", "round", items_field]);
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    if actual != expected
        || value["subject_id"].as_str() != Some(subject_id)
        || value["round"].as_u64() != Some(u64::from(round))
    {
        return Err(format!(
            "Round {items_field} set does not match its Subject and round"
        ));
    }
    value[items_field]
        .as_array()
        .map(Vec::len)
        .ok_or_else(|| format!("Round {items_field} set does not contain an array"))
}

fn prior_rows(ledger: &Ledger) -> Vec<serde_json::Value> {
    ledger
        .findings()
        .iter()
        .filter(|finding| {
            !finding.authority_diagnostic
                && !matches!(finding.status, Status::Rejected | Status::Wontfix)
        })
        .map(|finding| {
            let severity = format!("{:?}", finding.severity).to_lowercase();
            let effective_severity = finding
                .convergence_severity
                .map(|effective| format!("{effective:?}").to_lowercase());
            let scope = finding.convergence_scope_label();
            let mut row = serde_json::json!({
                "key": finding.key,
                "severity": severity,
                "status": finding.status.as_str(),
                "line": finding.identity_line,
                "title": finding.title,
                "body": finding.body,
                "source": finding.source,
                "last_seen_round": finding.last_seen_round,
            });
            let object = row.as_object_mut().expect("prior row is an object");
            let (file, line, location_unrecorded) =
                prior_location(&finding.identity_file, finding.identity_line);
            object.insert("file".into(), file);
            object.insert("line".into(), line);
            if location_unrecorded {
                object.insert("location_unrecorded".into(), serde_json::Value::Bool(true));
            }
            if scope != "in" {
                object.insert("scope".into(), serde_json::Value::String(scope.into()));
            }
            if effective_severity.as_deref() != Some(severity.as_str()) {
                object.insert(
                    "effective_severity".into(),
                    effective_severity.map_or(serde_json::Value::Null, serde_json::Value::String),
                );
            }
            row
        })
        .collect()
}

fn prior_location(file: &str, line: Option<i64>) -> (serde_json::Value, serde_json::Value, bool) {
    if file == review_core::legacy::CHANGE_WIDE_SENTINEL {
        return (serde_json::Value::Null, serde_json::Value::Null, false);
    }
    if review_core::is_valid_repo_path(file) {
        (
            serde_json::Value::String(file.to_string()),
            line.map_or(serde_json::Value::Null, |line| line.into()),
            false,
        )
    } else {
        (serde_json::Value::Null, serde_json::Value::Null, true)
    }
}

fn publish_snapshot(snapshot: &Snapshot, cas: &Cas) -> Result<(String, String), String> {
    let manifest_id = cas
        .put_json(&serde_json::to_value(&snapshot.manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let snapshot_id = cas
        .put_json(
            &snapshot
                .to_payload(Some(&manifest_id))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    Ok((snapshot_id, manifest_id))
}

fn authority_bytes(manifest: &Manifest, cas: &Cas, path: &str) -> Result<Vec<u8>, String> {
    let entry = manifest
        .get(path)
        .ok_or_else(|| format!("Authority Snapshot has no `{path}`"))?;
    if entry.kind == EntryKind::Symlink {
        return Err(format!("authority file `{path}` is a symlink"));
    }
    cas.get(&entry.content).map_err(|error| error.to_string())
}

fn captured_registry(
    manifest: &Manifest,
    cas: &Cas,
    registry: &str,
) -> Result<BTreeMap<String, BTreeMap<String, Vec<u8>>>, String> {
    let prefix = format!("{registry}/");
    let mut packages: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    for entry in &manifest.entries {
        let Some(relative) = entry.path.strip_prefix(&prefix) else {
            continue;
        };
        let Some((name, path)) = relative.split_once('/') else {
            continue;
        };
        if name.is_empty() || path.is_empty() {
            continue;
        }
        if entry.kind == EntryKind::Symlink {
            return Err(format!(
                "reviewer package `{name}` contains symlink `{}` in the Authority Snapshot",
                entry.path
            ));
        }
        packages.entry(name.to_string()).or_default().insert(
            path.to_string(),
            cas.get(&entry.content).map_err(|error| error.to_string())?,
        );
    }
    Ok(packages)
}

struct AuthorityLayout {
    root: String,
    lock: String,
    registry: String,
}

fn authority_layout(pipeline: &str) -> Result<AuthorityLayout, String> {
    let root = Path::new(pipeline)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::to_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "the pipeline path must live under `.af/pipelines/`".to_string())?;
    match root {
        ".af" => Ok(AuthorityLayout {
            root: root.to_string(),
            lock: ".af/af.lock".to_string(),
            registry: ".af/workers".to_string(),
        }),
        ".review" => Ok(AuthorityLayout {
            root: root.to_string(),
            lock: ".review/review.lock".to_string(),
            registry: ".review/reviewers".to_string(),
        }),
        _ => Err("the pipeline path must live under `.af/pipelines/`".to_string()),
    }
}

pub(crate) fn review_dir(pipeline: &str) -> Result<String, String> {
    authority_layout(pipeline).map(|layout| layout.root)
}

fn validate_af_project(bytes: &[u8], pipeline_path: &str) -> Result<(), String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("authority project `.af/af.toml` is not UTF-8: {error}"))?;
    let project: toml::Value = toml::from_str(text)
        .map_err(|error| format!("authority project `.af/af.toml`: {error}"))?;
    if project.get("version").and_then(toml::Value::as_integer) != Some(1) {
        return Err("authority project `.af/af.toml` must declare `version = 1`".to_string());
    }
    let selected = project
        .get("defaults")
        .and_then(|value| value.get("pipeline"))
        .and_then(toml::Value::as_str)
        .unwrap_or("review");
    if selected.is_empty()
        || !selected.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
        })
    {
        return Err("authority project default pipeline must be one safe name".to_string());
    }
    let selected_path = format!(".af/pipelines/{selected}.toml");
    if selected_path != pipeline_path {
        return Err(format!(
            "authority project selects pipeline `{selected_path}` but invocation requested `{pipeline_path}`"
        ));
    }
    Ok(())
}

fn validate_af_pipeline_pin(
    lockfile: &Lockfile,
    pipeline_path: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let name = Path::new(pipeline_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("authority pipeline has no UTF-8 name")?;
    let pin = lockfile
        .pipelines
        .get(name)
        .ok_or_else(|| format!("pipeline `{name}` is not pinned in `.af/af.lock`"))?;
    let found = review_store::canonical::blob_content_id(bytes);
    if pin.digest != found {
        return Err(format!(
            "pipeline `{name}` does not match `.af/af.lock`: locked {}, found {found}",
            pin.digest
        ));
    }
    Ok(())
}

fn authority_path(repo: &Path, pipeline: &Path) -> Result<String, String> {
    let relative: PathBuf = if pipeline.is_absolute() {
        let root = if repo.is_absolute() {
            repo.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| error.to_string())?
                .join(repo)
        };
        pipeline
            .strip_prefix(&root)
            .map_err(|_| "the pipeline path must be inside --repo")?
            .to_path_buf()
    } else {
        pipeline.to_path_buf()
    };
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => components.push(
                value
                    .to_str()
                    .ok_or("the pipeline path must be valid UTF-8")?,
            ),
            Component::CurDir => {}
            _ => return Err("the pipeline path must be repository-relative without `..`".into()),
        }
    }
    if components.is_empty() {
        return Err("the pipeline path is empty".into());
    }
    Ok(components.join("/"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn omitted_git_timeout_resolves_to_the_capture_default() {
        assert_eq!(
            super::requested_git_timeout(None).as_secs(),
            review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS
        );
        assert_eq!(
            super::requested_git_timeout(Some(std::time::Duration::from_secs(17))).as_secs(),
            17
        );
    }

    #[test]
    fn raw_patch_bound_refuses_before_base64_amplification() {
        let limit = super::maximum_raw_patch_bytes();
        assert!(!super::raw_patch_exceeds_change_set_bound(limit));
        assert!(super::raw_patch_exceeds_change_set_bound(limit + 1));
        assert!(limit * 4 / 3 >= review_core::MAX_CHANGE_SET_BYTES);
    }

    #[test]
    fn prior_findings_never_echo_a_path_live_admission_would_refuse() {
        assert_eq!(
            super::prior_location("src/main.rs", Some(7)),
            (
                serde_json::Value::String("src/main.rs".into()),
                serde_json::json!(7),
                false
            )
        );
        assert_eq!(
            super::prior_location("./src/main.rs", Some(7)),
            (serde_json::Value::Null, serde_json::Value::Null, true)
        );
        assert_eq!(
            super::prior_location(review_core::legacy::CHANGE_WIDE_SENTINEL, Some(7)),
            (serde_json::Value::Null, serde_json::Value::Null, false)
        );
    }
}
