//! Integration domain data and the complete post-apply Check sequence. Pure preparation
//! writes only CAS; execution owners supply current authority and publish returned events.
use super::*;

pub(crate) enum IntegrationSelection {
    Empty,
    Conflict(NewEvent),
    Candidates(SelectedIntegration),
}

pub(crate) struct SelectedIntegration {
    candidates: Vec<(IntegrationCandidateV1, String)>,
}

pub(crate) struct PreparedIntegration {
    pub(crate) plan_artifact_id: String,
    pub(crate) batch_id: String,
    pub(crate) derived_manifest: Manifest,
    pub(crate) derived_snapshot_id: String,
    selected: Vec<(IntegrationCandidateV1, String)>,
}

pub(crate) struct IntegrationCheckFailure {
    pub(crate) message: String,
    /// Every executed CheckResult retained before failure. A partial sequence is evidence,
    /// never an IntegrationChecks output or permission to promote.
    pub(crate) result_artifact_ids: Vec<String>,
}

pub(crate) struct IntegrationViews {
    pub(crate) finding_set_id: String,
    pub(crate) demand_set_id: String,
    pub(crate) semantic_closure_id: String,
}

pub(crate) struct IntegrationCommit {
    pub(crate) events: Vec<NewEvent>,
}

impl ReviewDomainState<'_> {
    pub(crate) fn select_integration(
        &self,
        policy: &review_config::IntegrationSpec,
        reviewer_execution: &BTreeMap<String, review_config::ReviewerExecutionSpec>,
        events: &[review_core::RunEvent],
        ledger: &Ledger,
    ) -> Result<IntegrationSelection, String> {
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
            if !reviewer_execution
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
                return Ok(IntegrationSelection::Conflict(
                    self.integration_conflict_event(
                        &[accepted.proposal_id],
                        &proposal.paths,
                        "Proposal changes a protected path",
                    )?,
                ));
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
                    return Ok(IntegrationSelection::Conflict(
                        self.integration_conflict_event(
                            std::slice::from_ref(&accepted.proposal_id),
                            &proposal.paths,
                            "Proposal claim no longer resolves in the exact Finding view",
                        )?,
                    ));
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
            return Ok(IntegrationSelection::Empty);
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
                    return Ok(IntegrationSelection::Conflict(
                        self.integration_conflict_event(
                            &[
                                selected[left].0.proposal_id.clone(),
                                selected[right].0.proposal_id.clone(),
                            ],
                            &overlap,
                            "selected Proposals overlap; semantic merging is forbidden",
                        )?,
                    ));
                }
            }
        }

        Ok(IntegrationSelection::Candidates(SelectedIntegration {
            candidates: selected,
        }))
    }

    pub(crate) fn prepare_integration(
        &self,
        policy: &review_config::IntegrationSpec,
        selection: &SelectedIntegration,
    ) -> Result<PreparedIntegration, String> {
        let selected = &selection.candidates;
        let mut entries: BTreeMap<String, review_source_git::Entry> = self
            .snapshot
            .entries
            .iter()
            .cloned()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        for (candidate, _) in selected {
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
        let derived_manifest =
            Manifest::new(entries.into_values().collect()).map_err(|error| error.to_string())?;
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
        Ok(PreparedIntegration {
            plan_artifact_id,
            batch_id,
            derived_manifest,
            derived_snapshot_id,
            selected: selection.candidates.clone(),
        })
    }

    /// Read the exact preparation admitted by Store. Selection is rederived from canonical
    /// events; Store independently proves composition before granting phase authority.
    pub(crate) fn read_prepared_integration(
        &self,
        policy: &review_config::IntegrationSpec,
        selected: &SelectedIntegration,
        plan_id: &str,
        snapshot_id: &str,
    ) -> Result<PreparedIntegration, String> {
        let plan: IntegrationPlanV1 =
            serde_json::from_value(self.cas.get_json(plan_id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        plan.validate()?;
        if plan.subject_id != self.authority.subject_id
            || plan.base_snapshot_id != self.authority.head_snapshot_id
            || plan.policy_id != self.authority.pipeline_policy_id
            || plan.protected_paths != policy.protected_paths
            || plan.candidates
                != selected
                    .candidates
                    .iter()
                    .map(|(c, _)| c.clone())
                    .collect::<Vec<_>>()
        {
            return Err(
                "Prepared Integration changed its captured policy or exact Proposal selection"
                    .into(),
            );
        }
        let derived_manifest: Manifest = serde_json::from_value(
            self.cas
                .get_json(&plan.derived_manifest_artifact_id)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        derived_manifest.validate().map_err(|e| e.to_string())?;
        let snapshot: SourceSnapshot =
            serde_json::from_value(self.cas.get_json(snapshot_id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let batch_id = format!("integration-{}", &plan_id[7..23]);
        if snapshot.artifact_manifest.as_ref() != Some(&plan.derived_manifest_artifact_id)
            || snapshot.parent_snapshot_id.as_ref() != Some(&self.authority.head_snapshot_id)
            || snapshot.content_digest != derived_manifest.content_digest()
            || !matches!(&snapshot.capture, SnapshotCapture::Derived{integration_batch_id,..} if integration_batch_id==&batch_id)
        {
            return Err("Prepared Integration changed its derived Snapshot".into());
        }
        Ok(PreparedIntegration {
            plan_artifact_id: plan_id.into(),
            batch_id,
            derived_manifest,
            derived_snapshot_id: snapshot_id.into(),
            selected: selected.candidates.clone(),
        })
    }

    pub(crate) fn integration_checks_event(
        &self,
        prepared: &PreparedIntegration,
        checks: &IntegrationChecksV1,
    ) -> Result<(String, NewEvent), String> {
        checks.validate()?;
        if checks.derived_snapshot_id != prepared.derived_snapshot_id {
            return Err("Integration checks name another prepared Snapshot".into());
        }
        let checks_artifact_id = self
            .cas
            .put_json(&serde_json::to_value(checks).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let mut refs = vec![
            checks_artifact_id.clone(),
            prepared.derived_snapshot_id.clone(),
        ];
        refs.extend(
            checks
                .checks
                .iter()
                .map(|check| check.result_artifact_id.clone()),
        );
        let event = NewEvent::new(
            EventType::IntegrationChecksCompletedV1,
            serde_json::to_value(IntegrationChecksCompletedPayloadV1 {
                batch_id: prepared.batch_id.clone(),
                checks_artifact_id: checks_artifact_id.clone(),
                passed: checks.passed(),
            })
            .map_err(|error| error.to_string())?,
        )
        .correlating(prepared.derived_snapshot_id.clone())
        .referencing(refs);
        Ok((checks_artifact_id, event))
    }

    pub(crate) fn prepare_integration_commit(
        &self,
        prepared: &PreparedIntegration,
        checks_artifact_id: &str,
        views: &IntegrationViews,
        ledger: &Ledger,
    ) -> Result<IntegrationCommit, String> {
        let selected = &prepared.selected;
        let derived_manifest = &prepared.derived_manifest;
        let derived_snapshot_id = &prepared.derived_snapshot_id;
        let plan_artifact_id = prepared.plan_artifact_id.clone();
        let batch_id = prepared.batch_id.clone();
        let checks_artifact_id = checks_artifact_id.to_owned();
        let finding_set_id = views.finding_set_id.clone();
        let demand_set_id = views.demand_set_id.clone();
        let semantic_closure_id = views.semantic_closure_id.clone();
        let current_subject: SubjectV1 = serde_json::from_value(
            self.cas
                .get_json(&self.authority.subject_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let derived_subject = match self.authority.subject_kind {
            review_core::SubjectKind::WholeTree => SubjectV1::whole_tree(derived_snapshot_id),
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
                let change_set = manifest_diff(&base_manifest, derived_manifest, self.cas)
                    .map_err(|error| error.to_string())?
                    .change_set(base_snapshot_id, derived_snapshot_id)?;
                let change_set_id = self
                    .cas
                    .put_json(&serde_json::to_value(change_set).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                SubjectV1::diff(derived_snapshot_id, base_snapshot_id, change_set_id)
            }
        };
        derived_subject.validate()?;
        let derived_subject_id = self
            .cas
            .put_json(&serde_json::to_value(&derived_subject).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;

        let mut by_finding: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> =
            BTreeMap::new();
        for (candidate, _) in selected {
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
            derived_subject_id,
            derived_snapshot_id.clone(),
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
        Ok(IntegrationCommit {
            events: commit_events,
        })
    }

    pub(crate) fn run_integration_checks_controlled(
        &self,
        policy: &review_config::IntegrationSpec,
        manifest: &Manifest,
        derived_snapshot_id: &str,
        deadline: Option<std::time::Instant>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<IntegrationChecksV1, IntegrationCheckFailure> {
        let binding = self
            .gate_execution
            .as_ref()
            .ok_or_else(|| IntegrationCheckFailure {
                message: "Integration requires the captured Gate Execution Binding".into(),
                result_artifact_ids: vec![],
            })?;
        IntegrationCheckSequence {
            cas: self.cas,
            checks: &self.checks,
            check_timeout: self.check_timeout,
            binding,
            container_provider: self.container_provider.as_ref(),
        }
        .run_recorded(
            policy,
            manifest,
            derived_snapshot_id,
            deadline,
            cancellation,
        )
    }
    fn integration_conflict_event(
        &self,
        proposal_ids: &[String],
        paths: &[String],
        reason: &str,
    ) -> Result<NewEvent, String> {
        Ok(NewEvent::new(
            EventType::IntegrationConflictV1,
            serde_json::to_value(IntegrationConflictPayloadV1 {
                base_snapshot_id: self.authority.head_snapshot_id.clone(),
                proposal_ids: proposal_ids.to_vec(),
                paths: paths.to_vec(),
                reason: reason.into(),
            })
            .map_err(|error| error.to_string())?,
        )
        .correlating(self.authority.subject_id.clone()))
    }
}

struct IntegrationCheckSequence<'a> {
    cas: &'a Cas,
    checks: &'a [CheckDefinition],
    check_timeout: Duration,
    binding: &'a review_config::GateExecutionSpec,
    container_provider: Option<&'a ContainerProvider>,
}
impl IntegrationCheckSequence<'_> {
    #[cfg(test)]
    fn run(
        &self,
        policy: &review_config::IntegrationSpec,
        manifest: &Manifest,
        derived_snapshot_id: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<IntegrationChecksV1, String> {
        self.run_recorded(policy, manifest, derived_snapshot_id, deadline, None)
            .map_err(|failure| failure.message)
    }
    fn run_recorded(
        &self,
        policy: &review_config::IntegrationSpec,
        manifest: &Manifest,
        derived_snapshot_id: &str,
        deadline: Option<std::time::Instant>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<IntegrationChecksV1, IntegrationCheckFailure> {
        let mut result_artifact_ids = Vec::new();
        let outcome = (|| -> Result<IntegrationChecksV1, String> {
            crate::task::control::check(cancellation)?;
            integration_remaining(self.check_timeout, deadline)?;
            let template = review_sandbox::SandboxTemplate::materialize(manifest, self.cas)
                .map_err(|error| error.to_string())?;
            let binding = self.binding;
            let container = match binding.provider {
                review_config::SandboxProviderSpec::TrustedLocal => None,
                review_config::SandboxProviderSpec::Container => Some(
                    self.container_provider
                        .cloned()
                        .unwrap_or_else(|| {
                            deadline.map_or_else(
                                ContainerProvider::detect,
                                ContainerProvider::detect_before,
                            )
                        })
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
            integration_remaining(self.check_timeout, deadline)?;
            let mut runner = CheckRunner::new(self.cas, sandbox.root())
                .with_timeout(self.check_timeout)
                .with_cancellation(cancellation);
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
                crate::task::control::check(cancellation)?;
                runner = runner.with_timeout(integration_remaining(self.check_timeout, deadline)?);
                let mut cleanup_failure = None;
                let result = match container.as_ref() {
                    Some(provider) => runner.run_with(definition, |program, args, env, timeout| {
                        match provider.exec_evidenced_controlled(
                            sandbox.root(),
                            program,
                            args,
                            env,
                            timeout,
                            cancellation,
                        ) {
                            Ok(execution) => Ok((execution.output, execution.stderr_held)),
                            Err(error) => {
                                if !error.cleanup_confirmed() {
                                    cleanup_failure = Some(error.to_string());
                                }
                                Err(error.to_string())
                            }
                        }
                    }),
                    None => runner.run(definition),
                };
                if let Some(error) = cleanup_failure {
                    let preserved = sandbox.root().to_path_buf();
                    // Preserve before any fallible evidence write: CAS failure must not delete a
                    // writable bind which may still have a daemon-owned process attached.
                    std::mem::forget(sandbox);
                    let evidence = match record_integration_check(self.cas, &result) {
                        Ok(id) => {
                            result_artifact_ids.push(id.clone());
                            format!("CheckResult retained as {id}")
                        }
                        Err(error) => format!("CheckResult could not be retained: {error}"),
                    };
                    return Err(format!(
                        "container cleanup was not confirmed; Integration sandbox preserved at {}: {error}; {evidence}",
                        preserved.display()
                    ));
                }
                let result_artifact_id = record_integration_check(self.cas, &result)?;
                result_artifact_ids.push(result_artifact_id.clone());
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
        })();
        outcome.map_err(|message| IntegrationCheckFailure {
            message,
            result_artifact_ids,
        })
    }
}

fn record_integration_check(
    cas: &Cas,
    result: &review_check::CheckResult,
) -> Result<String, String> {
    cas.put_json(&serde_json::to_value(result).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn integration_remaining(
    check_timeout: Duration,
    deadline: Option<std::time::Instant>,
) -> Result<Duration, String> {
    let remaining = deadline.map_or(check_timeout, |end| {
        check_timeout.min(end.saturating_duration_since(std::time::Instant::now()))
    });
    if remaining.is_zero() {
        Err("Integration exhausted its common Task Attempt deadline".into())
    } else {
        Ok(remaining)
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

#[cfg(test)]
mod tests;
