//! Canonical Review conclusion publication. The Task supplies its already-recorded spend; this
//! operation owns no Attempt or budget and cannot run automatic Integration.
use super::*;

struct TaskPublication<'a> {
    report_id: &'a str,
    accounting: review_core::TaskReviewAccountingV1,
    spent_tokens: review_core::task::usage::DecimalU128,
    resources_exhausted: bool,
    lease: &'a review_store::store::task::TaskLease,
    authority: &'a dyn review_store::store::task::TaskAuthority,
}

impl ReviewDomainState<'_> {
    /// The common scheduler report binds this canonical conclusion to one durable run.
    /// Repeating the same publication after a crash reads its original verdict without work.
    pub(crate) fn publish_task_report(
        &self,
        report: &RunReport,
        policy: ConvergencePolicy,
        task_report_id: &str,
        lease: &review_store::store::task::TaskLease,
        authority: &dyn review_store::store::task::TaskAuthority,
    ) -> Result<(RunVerdict, bool), String> {
        let captured = self
            .cas
            .get_artifact(task_report_id)
            .map_err(|e| e.to_string())?;
        if captured.artifact_type != review_core::task::report::TASK_RUN_REPORT_V1 {
            return Err("Canonical Task Review requires a typed scheduler report".into());
        }
        let state = self
            .store
            .lock()
            .expect("Task Store")
            .task_projection(self.cas, lease.task_id())
            .map_err(|e| e.to_string())?
            .ok_or("Review Task is absent")?;
        let accounting = review_core::TaskReviewAccountingV1 {
            task_id: lease.task_id().into(),
            task_revision_id: state.revision_id.clone(),
            plan_id: state.plan_id.clone().ok_or("Review Task has no plan")?,
            task_report_id: task_report_id.into(),
            through_sequence: state
                .next_sequence
                .checked_sub(1)
                .ok_or("Review Task has no prefix")?,
        };
        let execution = state
            .execution
            .as_ref()
            .ok_or("Review Task has no execution")?;
        let spent_tokens = execution.budget.committed_tokens().into();
        let resources_exhausted = execution.budget.breached();
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
                return Ok((
                    match verdict {
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
                    },
                    resources_exhausted,
                ));
            }
        }
        self.publish_report_inner(
            report,
            policy,
            TaskPublication {
                accounting,
                spent_tokens,
                resources_exhausted,
                report_id: task_report_id,
                lease,
                authority,
            },
        )
        .map(|verdict| (verdict, resources_exhausted))
    }

    fn publish_report_inner(
        &self,
        report: &RunReport,
        policy: ConvergencePolicy,
        task: TaskPublication<'_>,
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
        // Task acceptance requires all mandatory evidence even after budget exhaustion.
        let verdict = if !report.complete() {
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
        } else if task.resources_exhausted {
            RunVerdict::Fail(Verdict::Exhausted)
        } else {
            run_verdict(report, &convergence)
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
        let bindings = self
            .execution_bindings
            .lock()
            .expect("execution bindings")
            .values()
            .cloned()
            .collect();
        let execution = match &self.gate_execution {
            None => review_core::RunReportExecutionV6::Unbound {},
            Some(binding) if binding.caches.is_empty() => {
                review_core::RunReportExecutionV6::Bound {
                    execution_bindings: bindings,
                }
            }
            Some(_) => review_core::RunReportExecutionV6::Cached {
                execution_bindings: bindings,
                cache_snapshots: self
                    .cache_snapshots
                    .lock()
                    .expect("cache snapshots")
                    .values()
                    .cloned()
                    .collect(),
                cache_failures: self
                    .cache_failures
                    .lock()
                    .expect("cache failures")
                    .values()
                    .cloned()
                    .collect(),
            },
        };
        let refs = match &execution {
            review_core::RunReportExecutionV6::Cached {
                cache_snapshots, ..
            } => cache_snapshots
                .iter()
                .map(|snapshot| snapshot.source_digest.clone())
                .collect(),
            _ => Vec::new(),
        };
        let payload = review_core::RunReportPayloadV6 {
            outcomes,
            blocked_gates,
            verdict: persisted_verdict,
            spent_tokens: task.spent_tokens,
            task_accounting: task.accounting.clone(),
            execution,
        };
        let mut event = NewEvent::new(
            EventType::RunReportV6,
            serde_json::to_value(payload).map_err(|e| e.to_string())?,
        )
        .referencing(refs);
        event.artifact_refs.extend(
            task.accounting
                .artifact_refs()
                .into_iter()
                .map(String::from),
        );
        let event = self.bind_authority(event);
        let appended = self
            .store
            .lock()
            .expect("Task Store")
            .publish_task_review_report(self.cas, task.lease, task.report_id, event, task.authority)
            .map_err(|e| e.to_string())?;
        self.fold_appended_into_ledger_cache(&appended);
        *published = true;
        Ok(verdict)
    }
}
