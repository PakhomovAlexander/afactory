//! Machine-local Broker capabilities around one common started Attempt. Credentials and
//! connectors never enter captured packages, Worker context, or durable evidence.

use super::*;
use review_broker::{
    AuthorityError, BrokerHandle, Connector, Credential, ExactBroker, ExactBrokerClient,
    ExactReceiptSink, LeaseAuthority, ReceiptError,
};
use review_core::task::plan::{EffectiveWorkerBindingV1, ExecutionPlanV1};
use review_core::task::provider::TaskProviderProbePolicyV1;
use review_core::{BrokerLeaseV1, BrokerOperationPolicyV1, BrokerOperationReceiptV2};
use review_store::store::task::execution::broker::{BoundTaskBroker, TaskBrokerReceiptDisposition};
use std::sync::{Arc, OnceLock};

type ConnectorFactory = dyn Fn(u64) -> Result<Arc<dyn Connector>, String> + Send + Sync;

/// Installed host binding. Its factory receives the original absolute Attempt deadline;
/// the trusted connector must enforce that deadline in its transport. Merely checking a
/// lease before/after an unbounded synchronous network call cannot interrupt that call.
pub struct TaskBrokerProvider {
    identity: ProviderIdentity,
    credential: Vec<u8>,
    connector: Box<ConnectorFactory>,
}

enum ProviderIdentity {
    Worker(EffectiveWorkerBindingV1),
    Probe {
        policy_id: String,
        policy: TaskProviderProbePolicyV1,
    },
}

impl TaskBrokerProvider {
    pub fn new(
        binding: EffectiveWorkerBindingV1,
        credential: impl Into<Vec<u8>>,
        connector: impl Fn(u64) -> Result<Arc<dyn Connector>, String> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        binding.validate()?;
        let credential = credential.into();
        Credential::new(credential.clone()).map_err(|e| e.to_string())?;
        Ok(Self {
            identity: ProviderIdentity::Worker(binding),
            credential,
            connector: Box::new(connector),
        })
    }

    /// Readiness has its own captured policy. It never borrows a business Worker package,
    /// invocation policy, operation vector or reservation.
    pub fn for_probe(
        policy_id: String,
        policy: TaskProviderProbePolicyV1,
        credential: impl Into<Vec<u8>>,
        connector: impl Fn(u64) -> Result<Arc<dyn Connector>, String> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        policy.validate()?;
        if !review_core::is_digest(&policy_id) {
            return Err("Provider probe needs an exact policy identity".into());
        }
        let credential = credential.into();
        Credential::new(credential.clone()).map_err(|e| e.to_string())?;
        Ok(Self {
            identity: ProviderIdentity::Probe { policy_id, policy },
            credential,
            connector: Box::new(connector),
        })
    }
}

struct TaskBrokerBoundary<'a, 'store> {
    store: SharedEventStore<'store>,
    cas: &'a Cas,
    authority: &'a dyn TaskAuthority,
    bound: OnceLock<BoundTaskBroker>,
}

impl LeaseAuthority for TaskBrokerBoundary<'_, '_> {
    fn ensure_current(
        &self,
        lease: &BrokerLeaseV1,
        handle: &BrokerHandle,
    ) -> Result<(), AuthorityError> {
        let bound = self.bound.get().ok_or(AuthorityError)?;
        if bound.binding().lease != *lease || bound.binding().handle_id != handle.as_str() {
            return Err(AuthorityError);
        }
        self.store
            .lock()
            .map_err(|_| AuthorityError)?
            .check_task_broker_current(self.cas, bound, self.authority)
            .map_err(|_| AuthorityError)
    }
}

impl ExactReceiptSink for TaskBrokerBoundary<'_, '_> {
    fn record(&self, receipt: &BrokerOperationReceiptV2) -> Result<(), ReceiptError> {
        let bound = self.bound.get().ok_or(ReceiptError::Unavailable)?;
        match self
            .store
            .lock()
            .map_err(|_| ReceiptError::Unavailable)?
            .record_task_broker_receipt(self.cas, bound, receipt, self.authority)
            .map_err(|_| ReceiptError::Unavailable)?
        {
            TaskBrokerReceiptDisposition::Recorded => Ok(()),
            TaskBrokerReceiptDisposition::AuthorityRevoked => Err(ReceiptError::AuthorityRevoked),
        }
    }
}

fn failed(message: impl Into<String>, charged_tokens: Option<u128>) -> TaskWorkOutput {
    TaskWorkOutput {
        usage_observation: None,
        usage: None,
        outputs: Err(message.into()),
        charged_tokens,
        raw_artifact_ids: Vec::new(),
        usage_id: None,
        feedback_id: None,
    }
}

impl<'store, 'host> TaskRuntime<'store, 'host> {
    /// Install a local connector only for the exact captured slot binding. This changes no
    /// Pipeline policy, operation allowance or Provider identity.
    pub fn with_broker_provider(
        mut self,
        slot: &str,
        provider: &'host TaskBrokerProvider,
    ) -> Result<Self, String> {
        let plan: ExecutionPlanV1 =
            serde_json::from_value(envelope(self.cas, &self.plan_id)?.payload)
                .map_err(|e| e.to_string())?;
        if !matches!(&provider.identity, ProviderIdentity::Worker(binding) if plan.bindings.get(slot) == Some(binding))
            || self.broker_providers.contains_key(slot)
        {
            return Err("Task Broker provider differs from its exact captured slot binding".into());
        }
        self.broker_providers.insert(slot.into(), provider);
        Ok(self)
    }

    pub fn with_broker_probe(
        mut self,
        node: &str,
        provider: &'host TaskBrokerProvider,
    ) -> Result<Self, String> {
        let plan: ExecutionPlanV1 =
            serde_json::from_value(envelope(self.cas, &self.plan_id)?.payload)
                .map_err(|e| e.to_string())?;
        let Some(CompiledOperator::ProviderAdmissionBrokered {
            probe_policy_id, ..
        }) = self.graph.nodes.get(node).map(|node| &node.operator)
        else {
            return Err("Task Broker probe requires a captured Provider admission node".into());
        };
        let policy = super::provider::load_probe_policy(self.cas, &plan, probe_policy_id)?;
        if !matches!(&provider.identity, ProviderIdentity::Probe { policy_id, policy: local }
            if policy_id == probe_policy_id && *local == policy)
            || self.broker_probes.contains_key(node)
        {
            return Err("Task Broker probe differs from its exact captured policy".into());
        }
        self.broker_probes.insert(node.into(), provider);
        Ok(self)
    }

    pub(super) fn execute_host(
        &self,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        let policies = match self.host.broker_operations(self.cas, input) {
            Ok(policies) => policies,
            Err(error) => return failed(error, Some(0)),
        };
        let Some(policies) = policies else {
            return self
                .host
                .execute_with_broker(self.cas, input, attempt, None);
        };
        let Some(attempt) = attempt else {
            return failed("Task Broker requires a started common Attempt", Some(0));
        };
        self.execute_brokered(input, attempt, policies)
            .unwrap_or_else(|error| failed(error, Some(0)))
    }

    fn execute_brokered(
        &self,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        policies: Vec<BrokerOperationPolicyV1>,
    ) -> Result<TaskWorkOutput, String> {
        use review_core::task::pipeline::TaskOperatorV1;
        use review_graph::task::ReviewOperation;
        let resolved = self.resolve_node(&input.node)?;
        let provider = match &resolved.definition.operator {
            CompiledOperator::ReviewDomain {
                operation: ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
                ..
            }
            | CompiledOperator::Primitive {
                operator: TaskOperatorV1::Worker { slot } | TaskOperatorV1::Verify { slot },
                ..
            } => self.broker_providers.get(slot),
            CompiledOperator::ProviderAdmissionBrokered { .. } => {
                self.broker_probes.get(&input.node)
            }
            _ => {
                return Err(
                    "Task Broker operation has no captured Worker or Provider probe authority"
                        .into(),
                );
            }
        }
        .ok_or("Task Broker has no configured local provider")?;
        let lease = self
            .store
            .lock()
            .map_err(|_| "Task Store unavailable")?
            .task_broker_lease(self.cas, &self.lease, attempt, self.authority)
            .map_err(|e| e.to_string())?;
        let connector = (provider.connector)(attempt.reservation().deadline_unix_ms)?;
        let boundary = TaskBrokerBoundary {
            store: self.store.clone(),
            cas: self.cas,
            authority: self.authority,
            bound: OnceLock::new(),
        };
        let broker = ExactBroker::issue(
            lease,
            policies.clone(),
            Credential::new(provider.credential.clone()).map_err(|e| e.to_string())?,
            &boundary,
            connector.as_ref(),
            &boundary,
        )
        .map_err(|e| e.to_string())?;
        let bound = self
            .store
            .lock()
            .map_err(|_| "Task Store unavailable")?
            .bind_task_broker(
                self.cas,
                &self.lease,
                attempt,
                broker.handle().as_str(),
                &policies,
                self.authority,
            )
            .map_err(|e| e.to_string())?;
        boundary
            .bound
            .set(bound)
            .map_err(|_| "Task Broker handle was already bound")?;
        // Keep the concrete owner outside catch_unwind and all output handling. The opaque
        // client intentionally exposes no usage accessor to the Worker.
        let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.host
                .execute_with_broker(self.cas, input, Some(attempt), Some(&broker))
        }))
        .unwrap_or_else(|_| failed("Task operator panicked", None));
        broker.revoke();
        result.charged_tokens = Some(
            result
                .charged_tokens
                .unwrap_or(0)
                .max(broker.charged_usage()),
        );
        Ok(result)
    }
}
