//! Canonical Review conclusion publication. The execution owner supplies its already-recorded
//! spend; this operation owns no Attempt or budget and cannot run automatic Integration.
use super::*;

struct TaskPublication<'a> {
    report_id: &'a str,
    lease: &'a review_store::store::task::TaskLease,
    authority: &'a dyn review_store::store::task::TaskAuthority,
}

impl ReviewDomainState<'_> {
    pub(crate) fn publish_report(
        &self,
        report: &RunReport,
        policy: ConvergencePolicy,
        spent_tokens: Option<u64>,
    ) -> Result<RunVerdict, String> {
        self.publish_report_inner(report, policy, spent_tokens, None)
    }

    /// The common scheduler report binds this canonical conclusion to one durable run.
    /// Repeating the same publication after a crash reads its original verdict without work.
    pub(crate) fn publish_task_report(
        &self,
        report: &RunReport,
        policy: ConvergencePolicy,
        spent_tokens: Option<u64>,
        task_report_id: &str,
        lease: &review_store::store::task::TaskLease,
        authority: &dyn review_store::store::task::TaskAuthority,
    ) -> Result<RunVerdict, String> {
        let captured = self
            .cas
            .get_artifact(task_report_id)
            .map_err(|e| e.to_string())?;
        if captured.artifact_type != review_core::task::report::TASK_RUN_REPORT_V1 {
            return Err("Canonical Task Review requires a typed scheduler report".into());
        }
        for event in self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|e| e.to_string())?
        {
            if event.event_type.is_run_report()
                && event.causation_id.as_deref() == Some(&self.authority.round_event_id)
                && event.artifact_refs.iter().any(|id| id == task_report_id)
            {
                let verdict: RunVerdictV3 =
                    serde_json::from_value(event.payload["verdict"].clone())
                        .map_err(|e| e.to_string())?;
                return Ok(match verdict {
                    RunVerdictV3::Pass => RunVerdict::Pass,
                    RunVerdictV3::Fail {
                        reason: RunFailureReasonV3::Exhausted,
                    } => RunVerdict::Fail(Verdict::Exhausted),
                    RunVerdictV3::Fail { .. } => RunVerdict::Fail(Verdict::NotConverged),
                    RunVerdictV3::Incomplete { missing_nodes } => RunVerdict::Incomplete {
                        missing: missing_nodes
                            .into_iter()
                            .map(|entry| (entry.node, entry.reason))
                            .collect(),
                    },
                });
            }
        }
        self.publish_report_inner(
            report,
            policy,
            spent_tokens,
            Some(TaskPublication {
                report_id: task_report_id,
                lease,
                authority,
            }),
        )
    }

    fn publish_report_inner(
        &self,
        report: &RunReport,
        policy: ConvergencePolicy,
        spent_tokens: Option<u64>,
        task_publication: Option<TaskPublication<'_>>,
    ) -> Result<RunVerdict, String> {
        let mut published = self.report_published.lock().expect("report published");
        if *published {
            return Err("this kernel generation already published its conclusion".to_string());
        }
        let prior_conclusion = {
            let store = self.store.lock().expect("event store");
            let mut conclusion = false;
            for event in store
                .replay(&self.run_id)
                .map_err(|error| error.to_string())?
            {
                match event.event_type {
                    EventType::GenerationAdvancedV1 => conclusion = false,
                    event_type if event_type.is_run_report() => {
                        conclusion = run_report_closes_round(&event)
                            .map_err(|error| error.to_string())?
                            .unwrap_or(false);
                    }
                    _ => {}
                }
            }
            conclusion
        };
        if prior_conclusion {
            return Err("this campaign generation already has a durable conclusion".to_string());
        }
        // The guaranteed flush point. `run_gather` flushes when it runs — the ordinary case,
        // and the one that keeps attempt events ahead of the findings — but a gather that was
        // suppressed (a failed reviewer upstream) or a pipeline with no gather node never
        // reaches it, and the buffered attempts, charges included, would be lost. Every run
        // ends with a report, so flushing here records the paid work no matter the graph.
        self.flush_reviewer_events()?;
        let convergence = self.convergence(policy);
        // New Task acceptance requires all mandatory evidence even after budget exhaustion.
        // Frozen legacy conclusions retain their existing precedence.
        let verdict = if task_publication.is_some() && !report.complete() {
            RunVerdict::Incomplete {
                missing: report
                    .outcomes
                    .iter()
                    .filter_map(|(id, outcome)| match outcome {
                        NodeOutcome::Completed { .. } => None,
                        NodeOutcome::Failed { error, .. } => Some((id.clone(), error.clone())),
                        NodeOutcome::Suppressed { reason } => {
                            Some((id.clone(), format!("{reason:?}")))
                        }
                    })
                    .collect(),
            }
        } else {
            run_verdict(report, &convergence)
        };
        let append_report = |mut event: NewEvent| {
            if let Some(task) = &task_publication {
                event.artifact_refs.push(task.report_id.into());
                let event = self.bind_authority(event);
                let appended = self
                    .store
                    .lock()
                    .expect("Task Store")
                    .publish_task_review_report(
                        self.cas,
                        task.lease,
                        task.report_id,
                        event,
                        task.authority,
                    )
                    .map_err(|e| e.to_string())?;
                self.fold_appended_into_ledger_cache(&appended);
                return Ok(());
            }
            self.append(event)
        };
        let outcomes: Vec<RunNodeReportV2> = report
            .outcomes
            .iter()
            .map(|(id, outcome)| {
                let outcome = match outcome {
                    NodeOutcome::Completed { outputs } => RunNodeOutcomeV2::Completed {
                        output_artifacts: artifact_ids(outputs),
                    },
                    NodeOutcome::Failed { error, .. } => RunNodeOutcomeV2::Failed {
                        error: error.clone(),
                    },
                    NodeOutcome::Suppressed { reason } => RunNodeOutcomeV2::Suppressed {
                        reason: match reason {
                            review_graph::SuppressionReason::BranchNotSelected => {
                                // Legacy Review plans cannot contain conditional Task nodes.
                                // Their frozen report vocabulary has no inactive-branch outcome.
                                RunSuppressionReasonV2::UpstreamMissing
                            }
                            review_graph::SuppressionReason::GateBlocked => {
                                RunSuppressionReasonV2::GateBlocked
                            }
                            review_graph::SuppressionReason::UpstreamMissing => {
                                RunSuppressionReasonV2::UpstreamMissing
                            }
                        },
                    },
                };
                RunNodeReportV2 {
                    node: id.clone(),
                    outcome,
                }
            })
            .collect();
        let persisted_verdict =
            persisted_verdict(&verdict, &convergence, !report.blocked_gates.is_empty())?;
        let blocked_gates = report.blocked_gates.iter().cloned().collect();
        if self.gate_execution.is_some() {
            let execution_bindings: Vec<_> = self
                .execution_bindings
                .lock()
                .expect("execution bindings")
                .values()
                .cloned()
                .collect();
            let cache_snapshots: Vec<_> = self
                .cache_snapshots
                .lock()
                .expect("cache snapshots")
                .values()
                .cloned()
                .collect();
            let cache_failures: Vec<_> = self
                .cache_failures
                .lock()
                .expect("cache failures")
                .values()
                .cloned()
                .collect();
            let cache_requested = self
                .gate_execution
                .as_ref()
                .is_some_and(|binding| !binding.caches.is_empty());
            if !cache_requested {
                let payload = RunReportPayloadV4 {
                    outcomes,
                    blocked_gates,
                    verdict: persisted_verdict,
                    spent_tokens,
                    execution_bindings,
                };
                append_report(NewEvent::new(
                    EventType::RunReportV4,
                    serde_json::to_value(payload).map_err(|e| e.to_string())?,
                ))?;
            } else {
                let manifest_refs = cache_snapshots
                    .iter()
                    .map(|snapshot| snapshot.source_digest.clone())
                    .collect();
                let payload = RunReportPayloadV5 {
                    outcomes,
                    blocked_gates,
                    verdict: persisted_verdict,
                    spent_tokens,
                    execution_bindings,
                    cache_snapshots,
                    cache_failures,
                };
                append_report(
                    NewEvent::new(
                        EventType::RunReportV5,
                        serde_json::to_value(payload).map_err(|e| e.to_string())?,
                    )
                    .referencing(manifest_refs),
                )?;
            }
        } else {
            let payload = RunReportPayloadV3 {
                outcomes,
                blocked_gates,
                verdict: persisted_verdict,
                spent_tokens,
            };
            append_report(NewEvent::new(
                EventType::RunReportV3,
                serde_json::to_value(payload).map_err(|e| e.to_string())?,
            ))?;
        }
        *published = true;
        Ok(verdict)
    }
}
