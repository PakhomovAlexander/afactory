//! Common Review resumes its exact recorded Round before deciding whether any successor is
//! authorized. The legacy `prepare` entry point retains its historical progression policy.
use super::*;
use review_store::store::task::review_round_publication::TaskReviewRoundPublication;
#[cfg(test)]
mod tests;

pub(crate) struct PreparedRoundChange {
    pub events: Vec<NewEvent>,
    pub round_event_offset: usize,
}

/// Capture only data for the already authorized adjacent Round. Publication must use the
/// same Store permit; no Source or Round event is appended while Git or CAS work runs.
pub(crate) fn prepare_next_round(
    options: &Options,
    cas: &Cas,
    history: &[review_core::RunEvent],
    repo: &Repo,
    permit: &TaskReviewRoundPublication,
) -> Result<PreparedRoundChange, String> {
    let run_id = options
        .campaign
        .as_deref()
        .map(campaign_run_id)
        .unwrap_or_else(|| campaign_run_id("local"));
    if run_id != permit.campaign_id() {
        return Err("Review publication permit belongs to another Campaign".into());
    }
    if options.restart_round != permit.is_restart() {
        return Err(if permit.is_restart() {
            "an incomplete common Review Round requires explicit --restart-round"
        } else {
            "--restart-round requires an incomplete Round to supersede"
        }
        .into());
    }
    if history.len() as u64 != permit.next_review_sequence() {
        return Err("Review preparation lost its exact Campaign prefix".into());
    }
    let pipeline_path = authority_path(&options.repo, &options.pipeline)?;
    let campaign = resume(options, cas, history, &pipeline_path)?;
    if campaign.manifest_id != permit.predecessor().campaign_manifest_id {
        return Err("Review publication changed its original Campaign authority".into());
    }
    let projection =
        LedgerProjection::from_events(&run_id, history, cas).map_err(|error| error.to_string())?;
    if projection.ledger().round != permit.predecessor().round {
        return Err("Review preparation requires the exact predecessor Ledger generation".into());
    }
    let mut events = Vec::new();
    let (subject_id, subject, derived_manifest_id) = if let Some((_, committed)) =
        permit.integrated()
    {
        committed.validate()?;
        let subject: SubjectV1 = serde_json::from_value(
            cas.get_json(&committed.derived_subject_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        subject.validate()?;
        if subject.head_snapshot_id != committed.derived_snapshot_id
            || subject.kind != campaign.loaded.subject_kind()
            || subject.base_snapshot_id != campaign.manifest.base_snapshot_id
        {
            return Err(
                "committed Integration derived Subject contradicts Campaign authority".into(),
            );
        }
        let snapshot: SourceSnapshot = serde_json::from_value(
            cas.get_json(&subject.head_snapshot_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if !snapshot.is_derived()
            || snapshot.parent_snapshot_id.as_deref() != Some(committed.prior_snapshot_id.as_str())
        {
            return Err(
                "committed Integration does not name a derived child of its prior head".into(),
            );
        }
        let manifest_id = snapshot
            .artifact_manifest
            .as_deref()
            .ok_or("derived Snapshot has no exact Manifest")?;
        let manifest: Manifest = serde_json::from_value(
            cas.get_json(manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        manifest.validate().map_err(|error| error.to_string())?;
        if manifest.content_digest() != snapshot.content_digest {
            return Err("derived Snapshot Manifest contradicts its content digest".into());
        }
        (
            committed.derived_subject_id.clone(),
            subject,
            Some(manifest_id.to_string()),
        )
    } else {
        let captured = capture_source(options, cas, repo, &campaign)?;
        let mut refs = vec![
            campaign.manifest.authority_snapshot_id.clone(),
            campaign.manifest_id.clone(),
            captured.head_snapshot_id.clone(),
            captured.manifest_id.clone(),
        ];
        refs.extend(captured.change_set_id.clone());
        events.push(
            NewEvent::new(
                EventType::SourceCapturedV1,
                captured
                    .snapshot
                    .to_payload(Some(&captured.manifest_id))
                    .map_err(|error| error.to_string())?,
            )
            .caused_by(&campaign.opened_event_id)
            .correlating(&captured.head_snapshot_id)
            .referencing(refs),
        );
        let subject = match campaign.loaded.subject_kind() {
            SubjectKind::WholeTree => SubjectV1::whole_tree(&captured.head_snapshot_id),
            SubjectKind::Diff => SubjectV1::diff(
                &captured.head_snapshot_id,
                campaign
                    .manifest
                    .base_snapshot_id
                    .as_deref()
                    .ok_or("diff Campaign has no pinned Base Snapshot")?,
                captured
                    .change_set_id
                    .as_deref()
                    .ok_or("diff Campaign produced no Change Set")?,
            ),
        };
        subject.validate()?;
        let id = cas
            .put_json(&serde_json::to_value(&subject).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        (id, subject, None)
    };
    let prior_finding_set_id = if let Some(id) = permit.restart_prior_finding_set_id() {
        id.to_string()
    } else {
        let prior = serde_json::json!({"subject_id":subject_id,"round":permit.next_round(),
            "prior_findings":prior_rows(projection.ledger())});
        let prior_bytes = serde_json::to_string_pretty(&prior)
            .map_err(|error| error.to_string())?
            .len();
        if prior_bytes > MAX_PRIOR_FINDINGS_BYTES {
            return Err(format!(
                "exact prior Finding Set is {prior_bytes} bytes; maximum is {MAX_PRIOR_FINDINGS_BYTES} bytes and partitioning is required"
            ));
        }
        cas.put_json(&prior).map_err(|error| error.to_string())?
    };
    let payload = RoundStartedPayloadV1 {
        round: permit.next_round(),
        epoch: permit.next_epoch(),
        campaign_manifest_id: campaign.manifest_id.clone(),
        subject_id: subject_id.clone(),
        prior_finding_set_id,
        prior_demand_set_id: permit.prior_demand_set_id().to_string(),
    };
    payload.validate()?;
    if permit.is_restart() {
        let value = RoundInputSupersededPayloadV1 {
            round: permit.predecessor().round,
            old_epoch: permit.predecessor().epoch,
            new_epoch: permit.next_epoch(),
            campaign_manifest_id: campaign.manifest_id.clone(),
            old_subject_id: permit.predecessor().subject_id.clone(),
            replacement_subject_id: subject_id.clone(),
        };
        value.validate()?;
        let mut refs = vec![
            campaign.manifest.authority_snapshot_id.clone(),
            campaign.manifest_id.clone(),
            permit.predecessor().subject_id.clone(),
            subject_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        refs.extend(subject.base_snapshot_id.clone());
        refs.extend(subject.change_set_id.clone());
        events.push(
            NewEvent::new(
                EventType::RoundInputSupersededV1,
                serde_json::to_value(value).map_err(|error| error.to_string())?,
            )
            .caused_by(permit.predecessor_round_event_id())
            .correlating(&subject_id)
            .referencing(refs),
        );
    }
    let mut refs = vec![
        campaign.manifest.authority_snapshot_id.clone(),
        campaign.manifest_id.clone(),
        subject.head_snapshot_id.clone(),
        subject_id.clone(),
        payload.prior_finding_set_id.clone(),
        payload.prior_demand_set_id.clone(),
    ];
    refs.extend(derived_manifest_id);
    refs.extend(subject.base_snapshot_id);
    refs.extend(subject.change_set_id);
    let cause = if let Some((id, _)) = permit.integrated() {
        id
    } else if permit.is_restart() {
        permit.predecessor_round_event_id()
    } else {
        &campaign.opened_event_id
    };
    let round_event_offset = events.len();
    events.push(
        NewEvent::new(
            EventType::RoundStartedV1,
            serde_json::to_value(payload).map_err(|error| error.to_string())?,
        )
        .caused_by(cause)
        .correlating(subject_id)
        .referencing(refs),
    );
    if !permit.is_restart() {
        events.push(
            NewEvent::new(
                EventType::GenerationAdvancedV1,
                serde_json::json!({"round":permit.next_round()}),
            )
            .caused_by(
                permit
                    .event_id_at(round_event_offset)
                    .map_err(|error| error.to_string())?,
            ),
        );
    }
    Ok(PreparedRoundChange {
        events,
        round_event_offset,
    })
}

/// Validate captured Campaign options and hydrate one exact Round without capturing Source,
/// advancing the Ledger or publishing an event. A closed Round is deliberately readable.
pub(crate) fn prepare_recorded_round(
    options: &Options,
    cas: &Cas,
    store: &EventStore,
    round_event_id: &str,
) -> Result<PreparedRun, String> {
    let run_id = options
        .campaign
        .as_deref()
        .map(campaign_run_id)
        .unwrap_or_else(|| campaign_run_id("local"));
    let pipeline_path = authority_path(&options.repo, &options.pipeline)?;
    let events = store.replay(&run_id).map_err(|error| error.to_string())?;
    let campaign = resume(options, cas, &events, &pipeline_path)?;
    let event = events
        .iter()
        .find(|event| event.event_id == round_event_id)
        .filter(|event| event.event_type == EventType::RoundStartedV1)
        .ok_or("Task's exact recorded Review Round is absent from its Campaign")?;
    let payload: RoundStartedPayloadV1 =
        serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
    payload.validate()?;
    if payload.campaign_manifest_id != campaign.manifest_id {
        return Err("Task's recorded Round changed its captured Campaign manifest".into());
    }
    let snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&campaign.manifest.authority_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let projection =
        LedgerProjection::from_events(&run_id, &events, cas).map_err(|error| error.to_string())?;
    let prior_subject_id = original_prior_subject(&events, event, &payload)?;
    let round = load_round_with_prior_subject(
        options,
        cas,
        event.event_id.clone(),
        payload,
        (&snapshot.repository_id, &prior_subject_id),
        projection,
    )?;
    let authority = RoundAuthority::load_recorded(store, cas, &run_id, &round.event_id)?;
    let check_timeout = Duration::from_secs(
        campaign
            .manifest
            .check_timeout_seconds
            .unwrap_or(campaign.loaded.check_timeout_seconds()),
    );
    let git_timeout = Duration::from_secs(
        campaign
            .manifest
            .git_timeout_seconds
            .unwrap_or(review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS),
    );
    let convergence = options.mode.convergence(campaign.loaded.convergence());
    Ok(PreparedRun {
        loaded: campaign.loaded,
        snapshot: round.snapshot,
        run_id,
        focus: campaign.manifest.focus,
        timeout: Duration::from_secs(campaign.manifest.reviewer_timeout_seconds),
        check_timeout,
        git_timeout,
        convergence,
        authority,
        ledger_projection: round.ledger_projection,
    })
}

/// The same numerical Round retains its original prior sets through each exact supersession.
/// Their legacy raw headers therefore continue to name the first epoch's Subject.
fn original_prior_subject(
    events: &[review_core::RunEvent],
    event: &review_core::RunEvent,
    payload: &RoundStartedPayloadV1,
) -> Result<String, String> {
    let mut current = event;
    let mut started = payload.clone();
    while started.epoch > 1 {
        let previous = events
            .iter()
            .find(|candidate| {
                candidate.event_type == EventType::RoundStartedV1
                    && Some(candidate.event_id.as_str()) == current.causation_id.as_deref()
                    && candidate.sequence < current.sequence
            })
            .ok_or("restarted Review has no exact predecessor Round")?;
        let old: RoundStartedPayloadV1 =
            serde_json::from_value(previous.payload.clone()).map_err(|error| error.to_string())?;
        if old.round != started.round
            || old.epoch.checked_add(1) != Some(started.epoch)
            || old.campaign_manifest_id != started.campaign_manifest_id
            || old.prior_finding_set_id != started.prior_finding_set_id
            || old.prior_demand_set_id != started.prior_demand_set_id
        {
            return Err(
                "restarted Review changed its original prior sets or adjacent epoch".into(),
            );
        }
        let expected = RoundInputSupersededPayloadV1 {
            round: old.round,
            old_epoch: old.epoch,
            new_epoch: started.epoch,
            campaign_manifest_id: old.campaign_manifest_id.clone(),
            old_subject_id: old.subject_id.clone(),
            replacement_subject_id: started.subject_id.clone(),
        };
        let expected = serde_json::to_value(expected).map_err(|error| error.to_string())?;
        if !events.iter().any(|candidate| {
            candidate.event_type == EventType::RoundInputSupersededV1
                && candidate.sequence > previous.sequence
                && candidate.sequence < current.sequence
                && candidate.causation_id.as_deref() == Some(previous.event_id.as_str())
                && candidate.payload == expected
        }) {
            return Err("restarted Review lost its exact input supersession".into());
        }
        current = previous;
        started = old;
    }
    Ok(started.subject_id)
}

/// Source capture may create CAS objects, but never appends SourceCaptured or Round events.
pub(super) struct CapturedRoundSource {
    pub(super) snapshot: Snapshot,
    pub(super) head_snapshot_id: String,
    pub(super) manifest_id: String,
    pub(super) change_set_id: Option<String>,
}
pub(super) fn capture_source(
    options: &Options,
    cas: &Cas,
    repo: &Repo,
    campaign: &OpenCampaign,
) -> Result<CapturedRoundSource, String> {
    let capture = Capture::new(repo, cas);
    let candidate_ref = options.candidate.as_deref().unwrap_or("HEAD");
    let mut snapshot = if options.uncommitted {
        capture
            .dirty()
            .map_err(|error| format!("capturing revalidated worktree: {error}"))?
    } else {
        capture
            .committed(candidate_ref)
            .map_err(|error| format!("capturing candidate `{candidate_ref}`: {error}"))?
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
    let base_snapshot = if campaign.loaded.subject_kind() == SubjectKind::Diff {
        Some(
            serde_json::from_value::<SourceSnapshot>(
                cas.get_json(
                    campaign
                        .manifest
                        .base_snapshot_id
                        .as_deref()
                        .ok_or("diff Campaign has no pinned Base Snapshot")?,
                )
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    if let Some(base_snapshot) = &base_snapshot
        && snapshot.submodules != base_snapshot.submodules
    {
        let mut paths: Vec<String> = snapshot
            .submodules
            .iter()
            .chain(&base_snapshot.submodules)
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
        let base_snapshot = base_snapshot.as_ref().expect("diff Base was loaded");
        let base_manifest_id = base_snapshot
            .artifact_manifest
            .as_deref()
            .ok_or("Campaign Base Snapshot has no artifact manifest")?;
        let base_manifest: Manifest = serde_json::from_value(
            cas.get_json(base_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let base_tree = capture
            .rehydrate_committed(base_snapshot, &base_manifest)
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
    if tree_diff
        .as_ref()
        .is_some_and(|diff| diff.changes.is_empty())
    {
        return Err(
            "refusing empty Diff before Gates, Provider admission, or Worker dispatch; select a different Base/candidate or a whole-tree pipeline"
                .into(),
        );
    }
    match &tree_diff {
        Some(diff) => crate::run_progress(
            options,
            format_args!(
                "subject   Diff ({} changed records, {} patch bytes)",
                diff.changes.len(),
                diff.patch().len()
            ),
        ),
        None => crate::run_progress(options, format_args!("subject   WholeTree")),
    }
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
    Ok(CapturedRoundSource {
        snapshot,
        head_snapshot_id,
        manifest_id,
        change_set_id,
    })
}
