use super::*;
use review_broker::{Connector, ConnectorCall, ConnectorError, ConnectorReply, ExactBrokerClient};
use review_core::BrokerCredentialModeV1;
use review_core::task::broker::{TASK_BROKER_BINDING_V1, TaskBrokerBindingV1, TaskBrokerTargetV1};
use review_core::task::provider::{TASK_PROVIDER_ADMISSION_V2, TaskProviderProbePolicyV1};
use review_graph::task::CompiledOperator;
use review_pipeline::task::broker::TaskBrokerProvider;
use review_runner::task::provider::{PROBE_INPUT, TASK_PROVIDER_CONTEXT_V2, TaskProviderContextV2};
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use review_store::store::task::task_run_id;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const REVIEW_REPLY: &[u8] = br#"{"findings":[],"benchmark_demands":[],"dispositions":[]}"#;

struct Model {
    calls: AtomicUsize,
}
impl WorkerModelAdapter for Model {
    fn credential_mode(&self) -> BrokerCredentialModeV1 {
        BrokerCredentialModeV1::Brokered
    }
    fn provider_kind(&self) -> &'static str {
        "claude"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("claude-fixture".into(), "high".into()))
    }
    fn invoke(
        &self,
        _: &Cas,
        _: &std::path::Path,
        _: Vec<u8>,
        _: std::time::Duration,
        _: bool,
    ) -> ModelWorkerReturn {
        panic!("Brokered model cannot use ambient credentials")
    }
    fn invoke_with_broker(
        &self,
        cas: &Cas,
        _: &std::path::Path,
        input: Vec<u8>,
        timeout: std::time::Duration,
        writable: bool,
        broker: Option<&dyn ExactBrokerClient>,
    ) -> ModelWorkerReturn {
        assert!(!timeout.is_zero());
        let broker = broker.expect("only an installed, durably bound Broker may invoke this model");
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let returned = if n == 0 {
            assert!(!writable);
            assert_eq!(input, PROBE_INPUT);
            broker.call("capability", &input, 7)
        } else {
            assert_eq!(n, 1, "selected work must not be repeated");
            assert!(
                !writable,
                "Broker admission does not widen Claude's Review tool role"
            );
            assert!(
                String::from_utf8(input)
                    .unwrap()
                    .contains("Captured instruction marker.")
            );
            broker.call("inference", b"declared business request", 10)
        };
        let message = returned
            .map(|reply| reply.body)
            .map_err(|error| error.to_string());
        ModelWorkerReturn {
            usage_observation: None,
            raw_artifact_ids: message
                .as_ref()
                .ok()
                .map(|bytes| cas.put(bytes).unwrap())
                .into_iter()
                .collect(),
            message,
            // Connector evidence remains the charge floor even when native metadata is lower.
            usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
        }
    }
}

struct LocalConnector {
    probe: bool,
    accepted: bool,
    overrun: bool,
    calls: Arc<Mutex<Vec<bool>>>,
}
impl Connector for LocalConnector {
    fn execute(&self, call: ConnectorCall<'_>) -> Result<ConnectorReply, ConnectorError> {
        self.calls.lock().unwrap().push(self.probe);
        assert_eq!(call.method, "respond");
        assert_eq!(
            call.credential(),
            if self.probe {
                b"local-probe-credential".as_slice()
            } else {
                b"local-worker-credential".as_slice()
            }
        );
        if self.probe {
            assert_eq!(call.destination, "probe.test");
            assert_eq!(call.request, PROBE_INPUT);
            Ok(ConnectorReply::credential_free(
                if self.accepted {
                    b"OK".as_slice()
                } else {
                    b"Unavailable".as_slice()
                },
                if self.overrun { u64::MAX } else { 3 },
            ))
        } else {
            assert!(self.accepted && !self.overrun);
            assert_eq!(call.destination, "fixture.test");
            Ok(ConnectorReply::credential_free(REVIEW_REPLY, 5))
        }
    }
}

#[test]
fn provider_probe_and_worker_use_separate_captured_brokers_in_one_common_runtime() {
    check_probe_and_worker(false);
}

#[test]
fn brokered_provider_only_execution_retains_paid_refusals_and_overruns_before_review() {
    check_probe_and_worker(true);
}

fn check_probe_and_worker(provider_only: bool) {
    for (installed, accepted, overrun) in [
        (true, true, false),
        (true, false, false),
        (false, true, false),
        (true, true, true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let captured = broker::Captured::new_with_probe(&cas, &mut store);
        let model = Model {
            calls: AtomicUsize::new(0),
        };
        let (provider_node, probe_policy_id) = captured
            .graph
            .nodes
            .iter()
            .find_map(|(node, compiled)| {
                if let CompiledOperator::ProviderAdmissionBrokered {
                    probe_policy_id, ..
                } = &compiled.operator
                {
                    Some((node.clone(), probe_policy_id.clone()))
                } else {
                    None
                }
            })
            .unwrap();
        let probe_policy: TaskProviderProbePolicyV1 =
            serde_json::from_value(cas.get_artifact(&probe_policy_id).unwrap().payload).unwrap();
        let (slot, binding) = captured.plan.bindings.iter().next().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let deadlines = Arc::new(Mutex::new(Vec::new()));
        let make_connector = |probe| {
            let calls = calls.clone();
            let deadlines = deadlines.clone();
            move |deadline| {
                deadlines.lock().unwrap().push((probe, deadline));
                Ok(Arc::new(LocalConnector {
                    probe,
                    accepted,
                    overrun,
                    calls: calls.clone(),
                }) as Arc<dyn Connector>)
            }
        };
        let probe = TaskBrokerProvider::for_probe(
            probe_policy_id.clone(),
            probe_policy,
            b"local-probe-credential".to_vec(),
            make_connector(true),
        )
        .unwrap();
        let worker = TaskBrokerProvider::new(
            binding.clone(),
            b"local-worker-credential".to_vec(),
            make_connector(false),
        )
        .unwrap();
        let shared = SharedEventStore::new(&mut store);
        let host = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &captured.compiler,
            captured.lease.clone(),
            captured.models(&model),
        )
        .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&captured.compiler, &host, &NoTaskDeveloper);
        let runtime = TaskRuntime::with_store(
            shared.clone(),
            &cas,
            captured.lease.clone(),
            &authority,
            &host,
        )
        .unwrap();
        assert!(runtime.with_broker_probe(&provider_node, &worker).is_err());
        let runtime = TaskRuntime::with_store(
            shared.clone(),
            &cas,
            captured.lease.clone(),
            &authority,
            &host,
        )
        .unwrap();
        assert!(runtime.with_broker_provider(slot, &probe).is_err());
        let runtime = TaskRuntime::with_store(
            shared.clone(),
            &cas,
            captured.lease.clone(),
            &authority,
            &host,
        )
        .unwrap()
        .with_broker_provider(slot, &worker)
        .unwrap();
        let runtime = if installed {
            runtime.with_broker_probe(&provider_node, &probe).unwrap()
        } else {
            runtime
        };
        let complete = installed && accepted && !overrun;
        if provider_only {
            let doctor = runtime.execute_provider_admissions().unwrap();
            assert_eq!(doctor.ready(), complete, "{doctor:?}");
            assert_eq!(doctor.outcomes.len(), 1);
            assert_eq!(doctor.outcomes[0].0, provider_node);
            let projection = runtime.projection().unwrap();
            assert!(projection.run_reports.is_empty());
            let execution = projection.execution.unwrap();
            assert_eq!(
                execution.invocations.len(),
                1,
                "doctor did not dispatch Gates or Workers"
            );
            assert_eq!(execution.budget.begun_attempts(), 1);
            assert_eq!(
                execution.budget.committed_tokens(),
                if overrun {
                    u128::from(u64::MAX)
                } else if installed {
                    3
                } else {
                    0
                }
            );
            assert_eq!(model.calls.load(Ordering::SeqCst), usize::from(installed));
            assert_eq!(
                calls.lock().unwrap().as_slice(),
                if installed { &[true][..] } else { &[][..] }
            );
            assert!(host.selected_attempt_evidence().unwrap().is_empty());
        }
        // A fresh runtime has no in-memory pending output or prepared Attempt state.
        drop(runtime);
        let runtime = TaskRuntime::with_store(
            shared.clone(),
            &cas,
            captured.lease.clone(),
            &authority,
            &host,
        )
        .unwrap()
        .with_broker_provider(slot, &worker)
        .unwrap();
        let runtime = if installed {
            runtime.with_broker_probe(&provider_node, &probe).unwrap()
        } else {
            runtime
        };
        let report = runtime.execute().unwrap();
        if report.complete() != complete {
            let projection = runtime.projection().unwrap();
            for (_, output) in projection.execution.unwrap().outputs.values() {
                for port in output
                    .outputs
                    .values()
                    .filter(|p| p.artifact_type.contains("Gate"))
                {
                    for id in &port.artifact_ids {
                        let artifact = cas.get_artifact(id).unwrap();
                        eprintln!("Gate diagnostic: {}", artifact.payload);
                        if let Some(raw) = artifact
                            .payload
                            .get("gate_decision_id")
                            .and_then(|id| id.as_str())
                        {
                            eprintln!("Gate decision: {}", cas.get_json(raw).unwrap());
                        }
                    }
                }
            }
        }
        assert_eq!(report.complete(), complete, "{report:?}");
        let _ = runtime.execute();
        let expected_calls: &[bool] = if complete {
            &[true, false]
        } else if installed {
            &[true]
        } else {
            &[]
        };
        assert_eq!(calls.lock().unwrap().as_slice(), expected_calls);
        assert_eq!(
            model.calls.load(Ordering::SeqCst),
            if complete { 2 } else { usize::from(installed) }
        );
        let execution = runtime.projection().unwrap().execution.unwrap();
        let total = if overrun {
            u128::from(u64::MAX)
        } else if complete {
            8
        } else if installed {
            3
        } else {
            0
        };
        assert_eq!(execution.budget.committed_tokens(), total);
        let attempts = execution.attempt_accounting();
        let provider_attempt = attempts
            .iter()
            .find(|a| a.reservation.node == provider_node)
            .unwrap();
        assert_eq!(provider_attempt.reservation.tokens, 32);
        let run = task_run_id(&captured.task.task_id).unwrap();
        let records: Vec<TaskBrokerBindingV1> = shared
            .lock()
            .unwrap()
            .replay(&run)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == EventType::TaskBrokerTransitionV1)
            .filter_map(|event| {
                let id = event.payload["record_id"].as_str().unwrap();
                let artifact = cas.get_artifact(id).unwrap();
                (artifact.artifact_type == TASK_BROKER_BINDING_V1)
                    .then(|| serde_json::from_value(artifact.payload).unwrap())
            })
            .collect();
        assert_eq!(
            records.len(),
            if complete { 2 } else { usize::from(installed) }
        );
        for record in records {
            let attempt = attempts
                .iter()
                .find(|a| a.attempt_id == record.attempt_id)
                .unwrap();
            let is_probe = record.node == provider_node;
            assert_eq!(record.reservation_id, attempt.reservation.id);
            assert_eq!(
                record.lease.campaign_id,
                captured.compiler.round().binding().campaign_id
            );
            assert_eq!(
                record.lease.round_event_id,
                captured.compiler.round().binding().round_event_id
            );
            assert_eq!(
                record.lease.node_id,
                if is_probe {
                    provider_node.as_str()
                } else {
                    "reviewer"
                }
            );
            assert!(
                deadlines
                    .lock()
                    .unwrap()
                    .contains(&(is_probe, attempt.reservation.deadline_unix_ms))
            );
            if is_probe {
                assert_eq!(
                    record.target,
                    TaskBrokerTargetV1::ProviderAdmission {
                        probe_policy_id: probe_policy_id.clone()
                    }
                );
                let envelope = cas.get_artifact(&record.context_id).unwrap();
                assert_eq!(envelope.artifact_type, TASK_PROVIDER_CONTEXT_V2);
                let context: TaskProviderContextV2 =
                    serde_json::from_value(envelope.payload).unwrap();
                context.validate().unwrap();
                assert_eq!(envelope.input_artifacts, context.artifact_refs());
                assert_eq!(cas.get(&context.rendered_id).unwrap(), PROBE_INPUT);
                assert_eq!(
                    attempt.charged_tokens,
                    if overrun { u128::from(u64::MAX) } else { 3 }
                );
            } else {
                assert_eq!(
                    record.target,
                    TaskBrokerTargetV1::Worker {
                        slot: slot.clone(),
                        invocation_policy_id: binding.invocation_policy_id.clone()
                    }
                );
                assert_eq!(attempt.charged_tokens, 5);
            }
        }
        if complete {
            assert_eq!(
                execution.outputs[&provider_node].1.outputs["result"].artifact_type,
                TASK_PROVIDER_ADMISSION_V2
            );
        } else {
            assert!(!execution.outputs.contains_key(&provider_node));
        }
        let reopened = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        assert_eq!(
            reopened
                .task_projection(&cas, &captured.task.task_id)
                .unwrap()
                .unwrap()
                .execution
                .unwrap()
                .budget
                .committed_tokens(),
            total
        );
    }
}
