//! `af review ledger`, `show`, `export`, and the operator's Ledger dispositions: resolve, attest,
//! verify, challenge, policy time, group, evidence, and demand waivers.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use review_core::EventType;
use review_store::{Cas, EventStore, Ingest, LedgerProjection};

use crate::review::evidence::latest_round_evidence;
use crate::review::report::{
    findings_summary, findings_summary_line, human_duration, print_scope_authority_warnings,
    wall_span_ms,
};
use crate::{
    AttestChangeOptions, ChallengeResolutionOptions, DemandWaiveOptions, EvidenceAddOptions,
    EvidenceSatisfyOptions, ExportOptions, GroupOptions, LedgerOptions, PolicyTimeOptions,
    ResolveOptions, ShowOptions, VerifyFixOptions, authority, campaign_run_id, campaign_state,
    open_campaign_store, open_campaign_store_read_only,
};

pub(crate) fn print_ledger(options: &LedgerOptions) -> Result<(), String> {
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
            authority::serde_name(&finding.severity),
            finding.status.as_str(),
            finding.convergence_scope_label(),
            finding
                .convergence_severity
                .map(|severity| authority::serde_name(&severity))
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

pub(crate) fn export_proposal(options: &ExportOptions) -> Result<(), String> {
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

pub(crate) fn show(options: &ShowOptions) -> Result<(), String> {
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
        authority::serde_name(&finding.severity),
        finding
            .convergence_severity
            .map(|severity| authority::serde_name(&severity))
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
            authority::serde_name(&attached.severity),
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

pub(crate) fn resolve(options: &ResolveOptions) -> Result<(), String> {
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

pub(crate) fn attest_change(options: &AttestChangeOptions) -> Result<(), String> {
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

pub(crate) fn verify_fix(options: &VerifyFixOptions) -> Result<(), String> {
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

pub(crate) fn challenge_resolution(options: &ChallengeResolutionOptions) -> Result<(), String> {
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

pub(crate) fn advance_policy_time(options: &PolicyTimeOptions) -> Result<(), String> {
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

pub(crate) fn group(options: &GroupOptions, undo: bool) -> Result<(), String> {
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

pub(crate) fn add_evidence(options: &EvidenceAddOptions) -> Result<(), String> {
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

pub(crate) fn satisfy_evidence(options: &EvidenceSatisfyOptions) -> Result<(), String> {
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

pub(crate) fn waive_demand(options: &DemandWaiveOptions) -> Result<(), String> {
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
