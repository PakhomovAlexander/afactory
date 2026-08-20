//! Trusted campaign bootstrap and immutable Round input selection.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use review_config::Definition;
use review_config::lock::{Lockfile, Registry};
use review_core::{
    AuthorityFileV1, CampaignBudgetV1, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, CampaignReviewerV1, EventType, ReviewerPackageV1,
    RoundInputSupersededPayloadV1, RoundStartedPayloadV1, SourceSnapshot, SubjectKind, SubjectV1,
    run_report_closes_round,
};
use review_pipeline::RoundAuthority;
use review_source_git::{Capture, EntryKind, Manifest, Repo, Snapshot};
use review_store::{Cas, EventStore, Ingest, Ledger, NewEvent, Status};

use crate::{Options, campaign_run_id};

pub(super) struct PreparedRun {
    pub loaded: review_config::Loaded,
    pub snapshot: Manifest,
    pub prior_artifact: Option<String>,
    pub run_id: String,
    pub focus: Option<String>,
    pub authority: RoundAuthority,
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
        .unwrap_or_else(|| "run-local".to_string());
    let pipeline_path = authority_path(&options.repo, &options.pipeline)?;
    let events = store.replay(&run_id).map_err(|error| error.to_string())?;
    let campaign = if events.is_empty() {
        open_new(options, cas, store, repo, &run_id, &pipeline_path)?
    } else {
        resume(options, cas, &events, &pipeline_path)?
    };

    if campaign.loaded.subject_kind() == SubjectKind::Diff {
        return Err(
            "this kernel pinned the diff Campaign's Authority Snapshot and Base, but refuses \
             to execute until the typed Change Set is available; complete M2.3-M2.4"
                .to_string(),
        );
    }

    let round = prepare_round(options, cas, store, repo, &run_id, &campaign)?;
    let prior_artifact =
        (round.prior_count > 0).then(|| round.payload.prior_finding_set_id.clone());
    let authority = RoundAuthority::new(
        &round.event_id,
        &campaign.manifest.authority_snapshot_id,
        &campaign.manifest_id,
        &round.payload.subject_id,
    )?;
    Ok(PreparedRun {
        loaded: campaign.loaded,
        snapshot: round.snapshot,
        prior_artifact,
        run_id,
        focus: campaign.manifest.focus,
        authority,
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
    let (authority_snapshot_id, authority_manifest_id) = publish_snapshot(&snapshot, repo, cas)?;

    let review_dir = review_dir(pipeline_path)?;
    let lock_path = format!("{review_dir}/review.lock");
    let pipeline_bytes = authority_bytes(&snapshot.manifest, cas, pipeline_path)?;
    let lock_bytes = authority_bytes(&snapshot.manifest, cas, &lock_path)?;
    let pipeline_text = std::str::from_utf8(&pipeline_bytes)
        .map_err(|error| format!("authority pipeline `{pipeline_path}` is not UTF-8: {error}"))?;
    let lock_text = std::str::from_utf8(&lock_bytes)
        .map_err(|error| format!("authority lock `{lock_path}` is not UTF-8: {error}"))?;
    let lockfile = Lockfile::from_toml(lock_text).map_err(|error| error.to_string())?;
    let registry = Registry::captured(captured_registry(&snapshot.manifest, cas, &review_dir)?);
    let loaded = Definition::from_toml(pipeline_text)
        .map_err(|error| error.to_string())?
        .load_with(&lockfile, &registry)
        .map_err(|error| error.to_string())?;

    let pipeline_artifact_id = cas
        .put(&pipeline_bytes)
        .map_err(|error| error.to_string())?;
    let lock_artifact_id = cas.put(&lock_bytes).map_err(|error| error.to_string())?;
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
        project_policy_ids: Vec::new(),
        convergence: CampaignConvergenceV1 {
            clean_rounds: convergence.clean_rounds,
            max_rounds: convergence.max_rounds,
            gate: format!("{:?}", convergence.gate).to_lowercase(),
        },
        budgets,
        focus: options.focus.clone(),
        finding_identity_policy: "legacy-path-title@1".to_string(),
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
    println!("authority {authority_snapshot_id}");
    println!("manifest  {manifest_id}");
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
    for binding in &manifest.reviewers {
        let package: ReviewerPackageV1 = serde_json::from_value(
            cas.get_json(&binding.package_artifact_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        package.validate()?;
        if package.name != binding.name
            || package.version != binding.version
            || package.digest != binding.digest
        {
            return Err(format!(
                "captured reviewer package for node `{}` disagrees with CampaignManifest@1",
                binding.node
            ));
        }
        let mut files = BTreeMap::new();
        for (path, artifact_id) in package.files {
            files.insert(
                path,
                cas.get(&artifact_id).map_err(|error| error.to_string())?,
            );
        }
        if packages
            .insert(package.name.clone(), files.clone())
            .is_some_and(|prior| prior != files)
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
    println!("authority {} (pinned)", manifest.authority_snapshot_id);
    println!("manifest  {} (resumed)", payload.campaign_manifest_id);
    Ok(OpenCampaign {
        loaded,
        manifest,
        manifest_id: payload.campaign_manifest_id,
        opened_event_id: event.event_id.clone(),
    })
}

fn prepare_round(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
    run_id: &str,
    campaign: &OpenCampaign,
) -> Result<RoundInput, String> {
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    let mut closed_rounds = 0_u32;
    for event in &events {
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
        (Some((event, payload)), false) => load_round(cas, event.event_id.clone(), payload)?,
        (None, true) => {
            return Err("--restart-round requires an incomplete Round to supersede".into());
        }
        (existing, _restart) => capture_round(
            cas,
            store,
            repo,
            run_id,
            campaign,
            target_round,
            existing.as_ref().map(|(_, payload)| payload),
        )?,
    };

    {
        let mut ingest =
            Ingest::new(store, cas, run_id.to_string()).map_err(|error| error.to_string())?;
        while ingest.ledger().round < target_round {
            ingest.advance().map_err(|error| error.to_string())?;
        }
    }
    println!(
        "round    {} (epoch {})",
        round.payload.round, round.payload.epoch
    );
    if round.prior_count > 0 {
        println!("prior    {} findings carried", round.prior_count);
    }
    Ok(round)
}

fn capture_round(
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
    run_id: &str,
    campaign: &OpenCampaign,
    round: u32,
    superseded: Option<&RoundStartedPayloadV1>,
) -> Result<RoundInput, String> {
    let snapshot = Capture::new(repo, cas)
        .committed("HEAD")
        .map_err(|error| format!("capturing HEAD: {error}"))?;
    let (head_snapshot_id, manifest_id) = publish_snapshot(&snapshot, repo, cas)?;
    store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::SourceCapturedV1,
                snapshot.to_payload(&tree_id(&snapshot, repo)?, Some(&manifest_id)),
            )
            .caused_by(campaign.opened_event_id.clone())
            .correlating(head_snapshot_id.clone())
            .referencing(vec![
                campaign.manifest.authority_snapshot_id.clone(),
                campaign.manifest_id.clone(),
                head_snapshot_id.clone(),
                manifest_id,
            ]),
        )
        .map_err(|error| error.to_string())?;
    let subject = SubjectV1::whole_tree(&head_snapshot_id);
    subject.validate()?;
    let subject_id = cas
        .put_json(&serde_json::to_value(subject).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;

    let (prior_findings, demands) = if let Some(old) = superseded {
        let findings = cas
            .get_json(&old.prior_finding_set_id)
            .map_err(|error| error.to_string())?["prior_findings"]
            .clone();
        let current = serde_json::Value::Array(prior_rows(store, cas, run_id)?);
        if current != findings {
            return Err(
                "cannot supersede an incomplete Round after selected output changed the Ledger; \
                 start a new Campaign rather than carrying output across Subjects"
                    .into(),
            );
        }
        let demands = cas
            .get_json(&old.prior_demand_set_id)
            .map_err(|error| error.to_string())?["demands"]
            .clone();
        (findings, demands)
    } else {
        (
            serde_json::Value::Array(prior_rows(store, cas, run_id)?),
            serde_json::Value::Array(Vec::new()),
        )
    };
    let prior_count = prior_findings.as_array().map_or(0, Vec::len);
    let prior_finding_set_id = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id,
            "round": round,
            "prior_findings": prior_findings,
        }))
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
        epoch: superseded.map_or(1, |old| old.epoch + 1),
        campaign_manifest_id: campaign.manifest_id.clone(),
        subject_id: subject_id.clone(),
        prior_finding_set_id,
        prior_demand_set_id,
    };
    payload.validate()?;
    let causation_id = if let Some(old) = superseded {
        let superseded = RoundInputSupersededPayloadV1 {
            round,
            old_epoch: old.epoch,
            new_epoch: payload.epoch,
            campaign_manifest_id: campaign.manifest_id.clone(),
            old_subject_id: old.subject_id.clone(),
            replacement_subject_id: payload.subject_id.clone(),
        };
        superseded.validate()?;
        store
            .append(
                run_id,
                cas,
                NewEvent::new(
                    EventType::RoundInputSupersededV1,
                    serde_json::to_value(superseded).map_err(|error| error.to_string())?,
                )
                .caused_by(campaign.opened_event_id.clone())
                .correlating(payload.subject_id.clone())
                .referencing(vec![
                    campaign.manifest.authority_snapshot_id.clone(),
                    campaign.manifest_id.clone(),
                    old.subject_id.clone(),
                    payload.subject_id.clone(),
                    payload.prior_finding_set_id.clone(),
                    payload.prior_demand_set_id.clone(),
                ]),
            )
            .map_err(|error| error.to_string())?
            .event_id
    } else {
        campaign.opened_event_id.clone()
    };
    let started = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(&payload).map_err(|error| error.to_string())?,
            )
            .caused_by(causation_id)
            .correlating(subject_id)
            .referencing(vec![
                campaign.manifest.authority_snapshot_id.clone(),
                campaign.manifest_id.clone(),
                head_snapshot_id,
                payload.subject_id.clone(),
                payload.prior_finding_set_id.clone(),
                payload.prior_demand_set_id.clone(),
            ]),
        )
        .map_err(|error| error.to_string())?;
    println!("snapshot {}", snapshot.content_digest);
    Ok(RoundInput {
        payload,
        event_id: started.event_id,
        snapshot: snapshot.manifest,
        prior_count,
    })
}

fn load_round(
    cas: &Cas,
    event_id: String,
    payload: RoundStartedPayloadV1,
) -> Result<RoundInput, String> {
    payload.validate()?;
    let subject: SubjectV1 = serde_json::from_value(
        cas.get_json(&payload.subject_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    subject.validate()?;
    let snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&subject.head_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
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
    let prior_count = cas
        .get_json(&payload.prior_finding_set_id)
        .map_err(|error| error.to_string())?["prior_findings"]
        .as_array()
        .map_or(0, Vec::len);
    println!("snapshot {} (reused)", snapshot.content_digest);
    Ok(RoundInput {
        payload,
        event_id,
        snapshot: manifest,
        prior_count,
    })
}

fn prior_rows(
    store: &EventStore,
    cas: &Cas,
    run_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let ledger = Ledger::rebuild(store, cas, run_id).map_err(|error| error.to_string())?;
    Ok(ledger
        .findings()
        .iter()
        .filter(|finding| !matches!(finding.status, Status::Rejected | Status::Wontfix))
        .map(|finding| {
            serde_json::json!({
                "key": finding.key,
                "severity": format!("{:?}", finding.severity).to_lowercase(),
                "status": finding.status.as_str(),
                "file": finding.file,
                "line": finding.line,
                "title": finding.title,
                "body": finding.body,
                "source": finding.source,
                "last_seen_round": finding.last_seen_round,
            })
        })
        .collect())
}

fn publish_snapshot(
    snapshot: &Snapshot,
    repo: &Repo,
    cas: &Cas,
) -> Result<(String, String), String> {
    let manifest_id = cas
        .put_json(&serde_json::to_value(&snapshot.manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let snapshot_id = cas
        .put_json(&snapshot.to_payload(&tree_id(snapshot, repo)?, Some(&manifest_id)))
        .map_err(|error| error.to_string())?;
    Ok((snapshot_id, manifest_id))
}

fn tree_id(snapshot: &Snapshot, repo: &Repo) -> Result<String, String> {
    let revision = snapshot
        .source_revision
        .as_deref()
        .ok_or_else(|| "committed Snapshot has no source revision".to_string())?;
    repo.line(&["rev-parse", &format!("{revision}^{{tree}}")])
        .map_err(|error| error.to_string())
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
    review_dir: &str,
) -> Result<BTreeMap<String, BTreeMap<String, Vec<u8>>>, String> {
    let prefix = format!("{review_dir}/reviewers/");
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

fn review_dir(pipeline: &str) -> Result<String, String> {
    Path::new(pipeline)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::to_str)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            "the pipeline path must live under an authority review directory".to_string()
        })
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
