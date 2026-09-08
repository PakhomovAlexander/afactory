//! Integration: composing the selected, sealed, disjoint Proposals into one checked derived
//! Snapshot and advancing only the internal Campaign head (ADR-0040).

use std::collections::{BTreeMap, BTreeSet};

use review_check::{CheckRunner, CheckStatus};
use review_core::{
    Capture as SnapshotCapture, EventType, IntegrationCandidateV1, IntegrationCheckV1,
    IntegrationChecksCompletedPayloadV1, IntegrationChecksV1, IntegrationCommittedPayloadV1,
    IntegrationConflictPayloadV1, IntegrationPlanV1, IntegrationPreparedPayloadV1,
    NodeOutputReceiptPayloadV1, Producer, ProposalAcceptedPayloadV1, ProposalCandidateV1,
    RecordedSetPayloadV1, SourceSnapshot, SubjectV1,
};
use review_sandbox::{ContainerProvider, Mode, Sandbox};
use review_source_git::{Manifest, manifest_diff};
use review_store::NewEvent;

use crate::kernel::Kernel;

impl Kernel<'_> {
    /// Compose every eligible, disjoint Proposal into one checked internal Snapshot. This is
    /// deliberately callable for deterministic boundary tests; normal execution reaches it only
    /// after a passing terminal RunReport.
    pub fn integrate_selected_proposals(&self) -> Result<Option<String>, String> {
        let Some(policy) = self.integration.as_ref() else {
            return Ok(None);
        };
        let events = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?;
        let terminal = events.iter().rev().find(|event| {
            event.event_type.is_run_report()
                && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
        });
        if terminal.and_then(|event| event.payload.pointer("/verdict/kind"))
            != Some(&serde_json::Value::String("pass".into()))
        {
            return Err(
                "automatic Integration requires the current Round's passing conclusion".into(),
            );
        }
        if events.iter().any(|event| {
            event.event_type == EventType::IntegrationCommittedV1
                && event
                    .payload
                    .get("prior_subject_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(self.authority.subject_id.as_str())
        }) {
            return Ok(None);
        }

        let ledger = self.ledger();
        let mut report_to_finding = BTreeMap::new();
        for finding in ledger.finding_views() {
            for report in &finding.reports {
                report_to_finding.insert(report.report_id.clone(), finding.key.clone());
                if let Some(artifact_id) = &report.artifact_id {
                    report_to_finding.insert(artifact_id.clone(), finding.key.clone());
                }
            }
        }
        let priorities: BTreeMap<&str, u32> = policy
            .reviewer_priority
            .iter()
            .enumerate()
            .map(|(index, node)| (node.as_str(), index as u32))
            .collect();
        let default_priority = u32::try_from(priorities.len()).unwrap_or(u32::MAX);
        let mut selected = Vec::new();
        let mut seen_patches = BTreeSet::new();
        for event in events.iter().filter(|event| {
            event.event_type == EventType::ProposalAcceptedV1
                && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
        }) {
            let accepted: ProposalAcceptedPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            let node = event
                .node_id
                .as_deref()
                .ok_or("accepted Proposal has no producing node")?;
            let binding_node = self.reviewer_binding_node(node);
            if !self
                .reviewer_execution
                .get(&binding_node)
                .is_some_and(|binding| binding.auto_apply)
            {
                continue;
            }
            let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                self.cas
                    .get_json(&accepted.proposal_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            review_store::validate_envelope(&envelope)?;
            let proposal: review_core::PatchProposal =
                serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
            proposal.check_shape().map_err(str::to_string)?;
            if !proposal.auto_apply_nominated
                || proposal.base_snapshot_id != self.authority.head_snapshot_id
                || !seen_patches.insert(proposal.patch_artifact_id.clone())
            {
                continue;
            }
            if proposal.paths.iter().any(|path| {
                policy.protected_paths.iter().any(|protected| {
                    path == protected
                        || path
                            .strip_prefix(protected)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                })
            }) {
                self.record_integration_conflict(
                    &[accepted.proposal_id],
                    &proposal.paths,
                    "Proposal changes a protected path",
                )?;
                return Ok(None);
            }
            let candidate: ProposalCandidateV1 = serde_json::from_value(
                self.cas
                    .get_json(&accepted.candidate_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            candidate.validate().map_err(str::to_string)?;
            if candidate.base_snapshot_id != proposal.base_snapshot_id
                || candidate.patch_artifact_id != proposal.patch_artifact_id
                || candidate.paths != proposal.paths
            {
                return Err("accepted Proposal contradicts its sealed candidate".into());
            }
            let mut finding_ids = BTreeSet::new();
            for claim in &proposal.finding_refs {
                let finding = match claim.kind {
                    review_core::ClaimRefKind::Finding => {
                        ledger.finding_view(&claim.id).map(|finding| finding.key)
                    }
                    review_core::ClaimRefKind::Report => report_to_finding.get(&claim.id).cloned(),
                };
                let Some(finding) = finding else {
                    self.record_integration_conflict(
                        std::slice::from_ref(&accepted.proposal_id),
                        &proposal.paths,
                        "Proposal claim no longer resolves in the exact Finding view",
                    )?;
                    return Ok(None);
                };
                finding_ids.insert(finding);
            }
            for evidence in &proposal.evidence_ids {
                self.cas.verify(evidence).map_err(|error| {
                    format!("Proposal evidence `{evidence}` is not durable: {error}")
                })?;
            }
            selected.push((
                IntegrationCandidateV1 {
                    proposal_id: accepted.proposal_id,
                    candidate_artifact_id: accepted.candidate_artifact_id,
                    node_id: node.to_string(),
                    priority: priorities
                        .get(binding_node.as_str())
                        .copied()
                        .unwrap_or(default_priority),
                    patch_artifact_id: proposal.patch_artifact_id,
                    derived_manifest_artifact_id: candidate.derived_manifest_artifact_id,
                    paths: proposal.paths,
                    finding_ids: finding_ids.into_iter().collect(),
                    evidence_ids: proposal.evidence_ids,
                },
                accepted.proposal_artifact_id,
            ));
        }
        if selected.is_empty() {
            return Ok(None);
        }
        selected.sort_by(|left, right| {
            (&left.0.priority, &left.0.node_id, &left.0.proposal_id).cmp(&(
                &right.0.priority,
                &right.0.node_id,
                &right.0.proposal_id,
            ))
        });
        for left in 0..selected.len() {
            for right in left + 1..selected.len() {
                let overlap: Vec<String> = selected[left]
                    .0
                    .paths
                    .iter()
                    .filter(|a| selected[right].0.paths.iter().any(|b| paths_overlap(a, b)))
                    .cloned()
                    .collect();
                if !overlap.is_empty() {
                    self.record_integration_conflict(
                        &[
                            selected[left].0.proposal_id.clone(),
                            selected[right].0.proposal_id.clone(),
                        ],
                        &overlap,
                        "selected Proposals overlap; semantic merging is forbidden",
                    )?;
                    return Ok(None);
                }
            }
        }

        let mut entries: BTreeMap<String, review_source_git::Entry> = self
            .snapshot
            .entries
            .iter()
            .cloned()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        for (candidate, _) in &selected {
            let derived: Manifest = serde_json::from_value(
                self.cas
                    .get_json(&candidate.derived_manifest_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            derived.validate().map_err(|error| error.to_string())?;
            for path in &candidate.paths {
                match derived.get(path).cloned() {
                    Some(entry) => {
                        entries.insert(path.clone(), entry);
                    }
                    None => {
                        entries.remove(path);
                    }
                }
            }
        }
        let derived_manifest = Manifest::new_with_encoding(
            entries.into_values().collect(),
            self.snapshot.path_encoding,
        )
        .map_err(|error| error.to_string())?;
        let derived_manifest_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(&derived_manifest).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let plan = IntegrationPlanV1 {
            subject_id: self.authority.subject_id.clone(),
            base_snapshot_id: self.authority.head_snapshot_id.clone(),
            policy_id: self.authority.pipeline_policy_id.clone(),
            protected_paths: policy.protected_paths.clone(),
            candidates: selected.iter().map(|selected| selected.0.clone()).collect(),
            derived_manifest_artifact_id: derived_manifest_artifact_id.clone(),
        };
        plan.validate()?;
        let plan_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(&plan).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let batch_id = format!("integration-{}", &plan_artifact_id[7..23]);
        let prior_snapshot: SourceSnapshot = serde_json::from_value(
            self.cas
                .get_json(&self.authority.head_snapshot_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let derived_snapshot = SourceSnapshot {
            repository_id: prior_snapshot.repository_id,
            vcs: prior_snapshot.vcs,
            capture: SnapshotCapture::Derived {
                tree_id: derived_manifest.content_digest(),
                parent_snapshot_id: self.authority.head_snapshot_id.clone(),
                integration_batch_id: batch_id.clone(),
            },
            content_digest: derived_manifest.content_digest(),
            parent_snapshot_id: Some(self.authority.head_snapshot_id.clone()),
            source_revision: prior_snapshot.source_revision,
            artifact_manifest: Some(derived_manifest_artifact_id.clone()),
            submodules: prior_snapshot.submodules,
        };
        let derived_snapshot_id = self
            .cas
            .put_json(&serde_json::to_value(&derived_snapshot).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        self.append_integration_transition(
            NewEvent::new(
                EventType::IntegrationPreparedV1,
                serde_json::to_value(IntegrationPreparedPayloadV1 {
                    batch_id: batch_id.clone(),
                    plan_artifact_id: plan_artifact_id.clone(),
                    derived_snapshot_id: derived_snapshot_id.clone(),
                })
                .map_err(|error| error.to_string())?,
            )
            .correlating(self.authority.subject_id.clone())
            .referencing(vec![
                plan_artifact_id.clone(),
                derived_snapshot_id.clone(),
                derived_manifest_artifact_id,
            ]),
        )?;

        let checks =
            self.run_integration_checks(policy, &derived_manifest, &derived_snapshot_id)?;
        let passed = checks.passed();
        let result_ids: Vec<String> = checks
            .checks
            .iter()
            .map(|check| check.result_artifact_id.clone())
            .collect();
        let checks_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(&checks).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let mut check_refs = vec![checks_artifact_id.clone(), derived_snapshot_id.clone()];
        check_refs.extend(result_ids);
        self.append_integration_transition(
            NewEvent::new(
                EventType::IntegrationChecksCompletedV1,
                serde_json::to_value(IntegrationChecksCompletedPayloadV1 {
                    batch_id: batch_id.clone(),
                    checks_artifact_id: checks_artifact_id.clone(),
                    passed,
                })
                .map_err(|error| error.to_string())?,
            )
            .correlating(derived_snapshot_id.clone())
            .referencing(check_refs),
        )?;
        if !passed {
            return Ok(None);
        }

        let (finding_set_id, demand_set_id, semantic_closure_id) =
            current_integration_authority(&events, &self.authority.round_event_id)?;
        let current_subject: SubjectV1 = serde_json::from_value(
            self.cas
                .get_json(&self.authority.subject_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let derived_subject = match self.authority.subject_kind {
            review_core::SubjectKind::WholeTree => SubjectV1::whole_tree(&derived_snapshot_id),
            review_core::SubjectKind::Diff => {
                let base_snapshot_id = current_subject
                    .base_snapshot_id
                    .as_deref()
                    .ok_or("diff Integration has no Campaign Base")?;
                let base_snapshot: SourceSnapshot = serde_json::from_value(
                    self.cas
                        .get_json(base_snapshot_id)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let base_manifest: Manifest = serde_json::from_value(
                    self.cas
                        .get_json(
                            base_snapshot
                                .artifact_manifest
                                .as_deref()
                                .ok_or("Campaign Base has no Manifest")?,
                        )
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let change_set = manifest_diff(&base_manifest, &derived_manifest, self.cas)
                    .map_err(|error| error.to_string())?
                    .change_set(base_snapshot_id, &derived_snapshot_id)?;
                let change_set_id = self
                    .cas
                    .put_json(&serde_json::to_value(change_set).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                SubjectV1::diff(&derived_snapshot_id, base_snapshot_id, change_set_id)
            }
        };
        derived_subject.validate()?;
        let derived_subject_id = self
            .cas
            .put_json(&serde_json::to_value(&derived_subject).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;

        let mut by_finding: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> =
            BTreeMap::new();
        for (candidate, _) in &selected {
            for finding in &candidate.finding_ids {
                let grouped = by_finding.entry(finding.clone()).or_default();
                grouped.0.extend(candidate.paths.iter().cloned());
                grouped.1.extend(candidate.evidence_ids.iter().cloned());
            }
        }
        let mut commit_events = Vec::new();
        let mut attestation_ids = Vec::new();
        for (finding_id, (paths, evidence_ids)) in by_finding {
            let attestation = review_core::ChangeAttestationV1 {
                finding_id: finding_id.clone(),
                expected_finding_view_id: ledger
                    .finding_view_id(&finding_id)
                    .ok_or("Integration Finding view disappeared before commit")?,
                subject_id: self.authority.subject_id.clone(),
                change_set_id: self.authority.change_set_id.clone(),
                changed_regions: paths
                    .into_iter()
                    .map(|path| review_core::ChangedRegionV1 {
                        path,
                        start_line: None,
                        end_line: None,
                    })
                    .collect(),
                actor: "review.kernel/automatic-integration@1".into(),
                reason: format!("checked Integration batch {batch_id}"),
                evidence_ids: evidence_ids.into_iter().collect(),
            };
            attestation.validate()?;
            let mut inputs = selected
                .iter()
                .filter(|selected| selected.0.finding_ids.contains(&finding_id))
                .map(|selected| selected.1.clone())
                .collect::<Vec<_>>();
            inputs.extend(attestation.evidence_ids.iter().cloned());
            inputs.sort();
            inputs.dedup();
            let (record_id, _) = self
                .cas
                .put_artifact(
                    review_core::contract::CHANGE_ATTESTATION_V1,
                    Producer::KernelOperation {
                        run_id: self.run_id.clone(),
                        node_id: None,
                        operation_id: format!("automatic-integration:{batch_id}:{finding_id}"),
                    },
                    inputs,
                    Some(self.authority.head_snapshot_id.clone()),
                    serde_json::to_value(attestation).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            attestation_ids.push(record_id.clone());
            commit_events.push(
                NewEvent::new(
                    EventType::ChangeAttestedV1,
                    serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
                        artifact_id: record_id.clone(),
                    })
                    .map_err(|error| error.to_string())?,
                )
                .correlating(finding_id)
                .referencing(vec![record_id]),
            );
        }
        let proposal_ids = selected
            .iter()
            .map(|selected| selected.0.proposal_id.clone())
            .collect::<Vec<_>>();
        let committed = IntegrationCommittedPayloadV1 {
            batch_id,
            prior_subject_id: self.authority.subject_id.clone(),
            derived_subject_id: derived_subject_id.clone(),
            prior_snapshot_id: self.authority.head_snapshot_id.clone(),
            derived_snapshot_id: derived_snapshot_id.clone(),
            proposal_ids,
            attestation_ids: attestation_ids.clone(),
            expected_finding_set_id: finding_set_id.clone(),
            expected_demand_set_id: demand_set_id.clone(),
            policy_id: self.authority.pipeline_policy_id.clone(),
            semantic_closure_id: semantic_closure_id.clone(),
        };
        committed.validate()?;
        let mut commit_refs = vec![
            plan_artifact_id,
            checks_artifact_id,
            derived_subject_id.clone(),
            derived_snapshot_id,
            finding_set_id,
            demand_set_id,
            semantic_closure_id,
        ];
        commit_refs.extend(attestation_ids);
        commit_events.push(
            NewEvent::new(
                EventType::IntegrationCommittedV1,
                serde_json::to_value(committed).map_err(|error| error.to_string())?,
            )
            .correlating(self.authority.subject_id.clone())
            .referencing(commit_refs),
        );
        self.commit_integration(&commit_events)?;
        Ok(Some(derived_subject_id))
    }

    fn record_integration_conflict(
        &self,
        proposal_ids: &[String],
        paths: &[String],
        reason: &str,
    ) -> Result<(), String> {
        self.append_integration_transition(
            NewEvent::new(
                EventType::IntegrationConflictV1,
                serde_json::to_value(IntegrationConflictPayloadV1 {
                    base_snapshot_id: self.authority.head_snapshot_id.clone(),
                    proposal_ids: proposal_ids.to_vec(),
                    paths: paths.to_vec(),
                    reason: reason.into(),
                })
                .map_err(|error| error.to_string())?,
            )
            .correlating(self.authority.subject_id.clone()),
        )
    }

    fn run_integration_checks(
        &self,
        policy: &review_config::IntegrationSpec,
        manifest: &Manifest,
        derived_snapshot_id: &str,
    ) -> Result<IntegrationChecksV1, String> {
        let template = review_sandbox::SandboxTemplate::materialize(manifest, self.cas)
            .map_err(|error| error.to_string())?;
        let binding = self
            .gate_execution
            .as_ref()
            .ok_or("Integration requires the captured Gate Execution Binding")?;
        let container = match binding.provider {
            review_config::SandboxProviderSpec::TrustedLocal => None,
            review_config::SandboxProviderSpec::Container => Some(
                self.container_provider
                    .clone()
                    .unwrap_or_else(ContainerProvider::detect)
                    .with_image(
                        binding
                            .image
                            .as_deref()
                            .ok_or("container Integration binding has no pinned image")?,
                    ),
            ),
        };
        if let Some(provider) = container.as_ref()
            && !provider.availability().usable()
        {
            return Err(format!(
                "container provider unavailable: {}",
                provider.availability().reason()
            ));
        }
        let sandbox = match container.as_ref() {
            Some(provider) => provider
                .sandbox_from_template(&template, Mode::EphemeralWrite)
                .map_err(|error| error.to_string())?,
            None => Sandbox::from_template(&template, Mode::EphemeralWrite)
                .map_err(|error| error.to_string())?,
        };
        let runner = CheckRunner::new(self.cas, sandbox.root()).with_timeout(self.check_timeout);
        let selected: BTreeSet<_> = policy
            .post_apply_checks
            .iter()
            .map(String::as_str)
            .collect();
        let mut checks = Vec::new();
        for definition in self
            .checks
            .iter()
            .filter(|definition| selected.contains(definition.name.as_str()))
        {
            let result = match container.as_ref() {
                Some(provider) => runner.run_with(definition, |program, args, env, timeout| {
                    provider
                        .exec_evidenced(sandbox.root(), program, args, env, timeout)
                        .map(|execution| (execution.output, execution.stderr_held))
                        .map_err(|error| error.to_string())
                }),
                None => runner.run(definition),
            };
            let result_artifact_id = self
                .cas
                .put_json(&serde_json::to_value(&result).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
            checks.push(IntegrationCheckV1 {
                name: result.name,
                passed: result.status == CheckStatus::Passed,
                result_artifact_id,
            });
        }
        let checks = IntegrationChecksV1 {
            derived_snapshot_id: derived_snapshot_id.into(),
            checks,
        };
        checks.validate()?;
        Ok(checks)
    }
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn current_integration_authority(
    events: &[review_core::RunEvent],
    round_event_id: &str,
) -> Result<(String, String, String), String> {
    let mut finding_set = None;
    let mut demand_set = None;
    let mut semantic_closure = None;
    for event in events
        .iter()
        .filter(|event| event.causation_id.as_deref() == Some(round_event_id))
    {
        match event.event_type {
            EventType::NodeOutputReceiptV1 => {
                let receipt: NodeOutputReceiptPayloadV1 =
                    serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
                for port in receipt.outputs {
                    let target = if port.artifact_type == review_core::contract::FINDING_SET_V1 {
                        Some(&mut finding_set)
                    } else if port.artifact_type == review_core::contract::DEMAND_SET_V1 {
                        Some(&mut demand_set)
                    } else {
                        None
                    };
                    if let Some(target) = target {
                        if port.artifact_ids.is_empty() && port.optional {
                            continue;
                        }
                        let [artifact] = port.artifact_ids.as_slice() else {
                            return Err(format!(
                                "Integration authority port `{}` is not singular",
                                port.port
                            ));
                        };
                        *target = Some(artifact.clone());
                    }
                }
            }
            EventType::SemanticClosureCheckedV1 => {
                let recorded: RecordedSetPayloadV1 =
                    serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
                semantic_closure = Some(recorded.record_id);
            }
            _ => {}
        }
    }
    Ok((
        finding_set.ok_or("Integration has no exact current FindingSet@1")?,
        demand_set.ok_or("Integration has no exact current DemandSet@1")?,
        semantic_closure.ok_or("Integration has no SemanticClosure@1 proof")?,
    ))
}
