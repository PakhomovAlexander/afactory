//! Provider doctor executes only the installed, input-free admission operations. This is
//! not a partial graph result: ordinary Review still owns its original complete report.
use super::*;
use review_graph::NodeOutcome;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskProviderAdmissionReport {
    pub outcomes: Vec<(String, NodeOutcome)>,
}

impl TaskProviderAdmissionReport {
    /// Every capability produced its selected receipt. Overall Task resources and business
    /// acceptance remain separate; late usage never erases a previously selected receipt.
    pub fn ready(&self) -> bool {
        self.outcomes
            .iter()
            .all(|(_, outcome)| matches!(outcome, NodeOutcome::Completed { .. }))
    }
}

impl TaskRuntime<'_, '_> {
    /// Probe every captured Provider admission under its original common Attempt. No caller
    /// can name a subset or cause a Gate, business Worker or root operation to execute here.
    /// The selected admission outputs remain in the same Task for a later ordinary execute.
    /// No TaskRunReport or TaskResult is fabricated for the unexecuted business graph.
    pub fn execute_provider_admissions(&self) -> Result<TaskProviderAdmissionReport, String> {
        if self.integration.is_some() {
            return Err("Integration phase cannot execute Provider admission".into());
        }
        self.store
            .lock()
            .expect("Task Store")
            .check_current_task_plan_for_recording(self.cas, &self.lease, self.authority)
            .map_err(|e| e.to_string())?;
        if self.projection()?.plan_id.as_deref() != Some(&self.plan_id) {
            return Err("Provider admission runtime changed its captured Task plan".into());
        }
        let planned = self.graph.scheduler_plan()?;
        let mut nodes = Vec::new();
        for id in &self.graph.order {
            let definition = &self.graph.nodes[id];
            if !matches!(
                definition.operator,
                CompiledOperator::ProviderAdmission { .. }
                    | CompiledOperator::ProviderAdmissionBrokered { .. }
            ) {
                continue;
            }
            if !definition.inputs.is_empty()
                || !definition.contract.inputs.is_empty()
                || !definition.conditions.is_empty()
                || !planned.dependencies_of(id).is_empty()
                || !self.graph.allowances.contains_key(id)
            {
                return Err(
                    "Provider admission must retain its captured input-free Attempt contract"
                        .into(),
                );
            }
            nodes.push(&planned.nodes[id]);
        }
        lease::with_heartbeat_controlled(
            &self.store,
            self.cas,
            &self.lease,
            self.cancellation,
            || {
                let mut outcomes = Vec::new();
                // These independent, bounded probes deliberately occupy at most one slot. Their
                // reservations, original scopes, retries, failures and usage use normal Dispatch.
                for node in nodes {
                    let inputs = ArtifactMap::new();
                    let result = (|| {
                        self.record_invocation(node, &inputs)?;
                        let outputs = self.run(node, &inputs)?;
                        self.record_outputs(node, &outputs)?;
                        Ok(outputs)
                    })();
                    outcomes.push((
                        node.id.clone(),
                        match result {
                            Ok(outputs) => NodeOutcome::Completed { outputs },
                            Err(error) => NodeOutcome::Failed {
                                error,
                                class: self.failure_class(&node.id),
                            },
                        },
                    ));
                }
                Ok(TaskProviderAdmissionReport { outcomes })
            },
        )
    }
}
