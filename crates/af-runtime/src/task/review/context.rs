use super::*;

pub(super) fn compact_subject(
    cas: &Cas,
    bound: &ResolvedReviewSubject,
) -> Result<TaskReviewSubject, String> {
    let change_scope = bound
        .change_set
        .as_ref()
        .map(|changes| {
            // The authority limit applies to the original ChangeSet JSON, not merely decoded bytes.
            let id = bound
                .subject
                .change_set_id
                .as_ref()
                .ok_or("Diff lacks ChangeSet")?;
            cas.get_bounded(id, af_contracts::MAX_CHANGE_SET_BYTES as u64)
                .map_err(|e| e.to_string())?;
            let bytes = changes.canonical_patch()?;
            let content_id = cas.put(&bytes).map_err(|e| e.to_string())?;
            Ok::<_, String>(TaskReviewChangeScopeV1 {
                changed_paths: changes.changed_paths.clone(),
                renames: changes.renames.clone(),
                rename_detection_truncated: changes.rename_detection_truncated,
                git_version: changes.git_version.clone(),
                diff_policy_version: changes.diff_policy_version.clone(),
                patch: TaskReviewFileV1 {
                    path: TaskReviewFileV1::path_for(&content_id),
                    content_id,
                    bytes: bytes.len() as u64,
                },
            })
        })
        .transpose()?;
    let value = TaskReviewSubject {
        schema: "af.task-review-subject/3".into(),
        subject_id: bound.subject_id.clone(),
        subject: bound.subject.clone(),
        snapshot_id: bound.snapshot_id.clone(),
        prior_history_id: bound.prior_history_id.clone(),
        continuation_id: bound.continuation_id.clone(),
        round: bound.round,
        change_scope,
    };
    value.validate()?;
    Ok(value)
}

pub(super) fn expand_subject(
    cas: &Cas,
    value: TaskReviewSubject,
) -> Result<ResolvedReviewSubject, String> {
    value.validate()?;
    let changes = value
        .subject
        .change_set_id
        .as_ref()
        .map(|id| {
            let bytes = cas
                .get_bounded(id, af_contracts::MAX_CHANGE_SET_BYTES as u64)
                .map_err(|e| e.to_string())?;
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())
        })
        .transpose()?;
    let bound = ResolvedReviewSubject {
        subject_id: value.subject_id.clone(),
        subject: value.subject.clone(),
        change_set: changes,
        snapshot_id: value.snapshot_id.clone(),
        prior_history_id: value.prior_history_id.clone(),
        continuation_id: value.continuation_id.clone(),
        round: value.round,
    };
    bound.validate()?;
    if compact_subject(cas, &bound)? != value {
        return Err("Review file or scope changed its exact captured ChangeSet".into());
    }
    Ok(bound)
}

impl ReviewTaskDomain {
    pub(super) fn read_subject(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<ResolvedReviewSubject, String> {
        let (_, value) = self.value(cas, input, "subject", TASK_REVIEW_SUBJECT_V3)?;
        expand_subject(cas, value)
    }

    pub(super) fn assignment(
        &self,
        ledger: &Ledger,
        subject: &ResolvedReviewSubject,
        reviewer: &str,
    ) -> Result<TaskReviewAssignmentV1, String> {
        // Match the canonical prior-row exclusions, then narrow to the original logical source.
        let eligible: BTreeSet<_> = ledger
            .finding_views()
            .iter()
            .filter(|finding| {
                !finding.authority_diagnostic
                    && !matches!(
                        finding.status,
                        af_store::Status::Rejected | af_store::Status::Wontfix
                    )
            })
            .map(|finding| finding.key.clone())
            .collect();
        let value = TaskReviewAssignmentV1 {
            subject_id: subject.subject_id.clone(),
            prior_history_id: subject.prior_history_id.clone(),
            round: subject.round,
            reviewer: reviewer.into(),
            findings: crate::finding_set_entries(ledger)
                .into_iter()
                .filter(|finding| {
                    finding.source == reviewer && eligible.contains(&finding.finding_id)
                })
                .collect(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(super) fn bind_outputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let bound = self.bind_subject(cas, input)?;
        let snapshot = Some(bound.snapshot_id.as_str());
        let compact = compact_subject(cas, &bound)?;
        let mut refs = vec![bound.subject_id.clone()];
        if let Some(scope) = &compact.change_scope {
            refs.extend([
                compact.subject.change_set_id.clone().unwrap(),
                scope.patch.content_id.clone(),
            ]);
        }
        let mut outputs = BTreeMap::from([(
            "subject".into(),
            self.put(cas, input, TASK_REVIEW_SUBJECT_V3, snapshot, &compact, refs)?,
        )]);
        let ledger = self.prior_ledger(cas, input, &bound, 0)?;
        for name in self.policy.reviewers.keys() {
            let assignment = self.assignment(&ledger, &bound, name)?;
            outputs.insert(
                name.clone(),
                self.put(
                    cas,
                    input,
                    TASK_REVIEW_ASSIGNMENT_V1,
                    snapshot,
                    &assignment,
                    vec![bound.subject_id.clone()],
                )?,
            );
        }
        Ok(outputs)
    }

    pub(super) fn current_assignment(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        subject: &ResolvedReviewSubject,
    ) -> Result<TaskReviewAssignmentV1, String> {
        let (_, assignment): (_, TaskReviewAssignmentV1) =
            self.value(cas, input, "assignment", TASK_REVIEW_ASSIGNMENT_V1)?;
        assignment.validate()?;
        let address = self
            .input_addresses(input)?
            .get("assignment")
            .ok_or("Reviewer has no captured assignment binding")?;
        let ledger = self.prior_ledger(cas, input, subject, 0)?;
        if !self.policy.reviewers.contains_key(&address.port)
            || assignment != self.assignment(&ledger, subject, &address.port)?
        {
            return Err(
                "Reviewer assignment differs from its exact current source-scoped Findings".into(),
            );
        }
        Ok(assignment)
    }

    pub(super) fn validate_stage(
        &self,
        stage: &af_contracts::ReviewerStageOutput,
        assignment: &TaskReviewAssignmentV1,
    ) -> Result<(), String> {
        let ids = assignment
            .findings
            .iter()
            .map(|f| f.finding_id.clone())
            .collect::<Vec<_>>();
        crate::reviewer_result_value(stage, &ids)
            .map(|_| ())
            .map_err(|e| format!("Reviewer disposition coverage: {e:?}"))
    }

    pub(super) fn validate_result_assignment(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        name: &str,
        result: &af_contracts::ArtifactEnvelope,
        expected: &TaskReviewAssignmentV1,
    ) -> Result<(), String> {
        let worker = &self.graph.nodes[&self.graph.nodes[&input.node].inputs[name].node];
        let address = &worker.inputs["assignment"];
        self.validate_assignment_provenance(cas, input, result, expected, address)
    }

    pub(super) fn validate_assignment_provenance(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        result: &af_contracts::ArtifactEnvelope,
        expected: &TaskReviewAssignmentV1,
        address: &af_graph::task::Address,
    ) -> Result<(), String> {
        let expected_run = match invocation_producer(cas, input, None)? {
            Producer::KernelOperation { run_id, .. } => run_id,
            _ => unreachable!(),
        };
        let mut found = 0;
        for id in &result.input_artifacts {
            // Other exact refs may be non-envelope CAS blobs (package inputs).
            let value = cas.get_json(id).map_err(|e| e.to_string())?;
            if value.get("type").and_then(serde_json::Value::as_str)
                != Some(TASK_REVIEW_ASSIGNMENT_V1)
            {
                continue;
            }
            let artifact = envelope(cas, id)?;
            let actual: TaskReviewAssignmentV1 =
                serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
            if &actual != expected
                || artifact.subject_snapshot_id != result.subject_snapshot_id
                || artifact.producer
                    != (Producer::KernelOperation {
                        run_id: expected_run.clone(),
                        node_id: Some(address.node.clone()),
                        operation_id: "task-builtin@1".into(),
                    })
            {
                return Err(
                    "Selected Review result changed its exact assignment provenance".into(),
                );
            }
            found += 1;
        }
        if found != 1 {
            return Err("Selected Review result must retain exactly its own assignment".into());
        }
        Ok(())
    }
}
