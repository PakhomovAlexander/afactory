//! The model Worker transport forwards the exact captured context and the host's cancellation,
//! but no sandbox-local environment, to its adapter and retains usage and raw evidence on every
//! outcome. The adapter is in-process; no Provider, subprocess, or Task scheduler is involved.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use review_core::CredentialModeV1;
use review_core::task::execution::TaskInvocationV1;
use review_core::task::feedback::TaskFeedbackCodeV1;
use review_core::task::usage::TaskTokenUsageV3;
use review_runner::task::{
    ModelWorkerReturn, WorkerAccess, WorkerContract, WorkerModelAdapter, invoke_model,
};
use review_store::Cas;
use serde_json::json;

const VALID_REPLY: &[u8] =
    br#"{"schema":"af.worker-reply/1","outputs":{"document":[{"text":"Captured answer"}]}}"#;
const RAW: &[u8] = b"raw adapter evidence";
const TIMEOUT: Duration = Duration::from_millis(1234);

struct Fixture {
    directory: tempfile::TempDir,
    cas: Cas,
    contract: WorkerContract,
    context_id: String,
    input: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let contract = WorkerContract::capture(
            &cas,
            json!({"type":"object","additionalProperties":false}),
            BTreeMap::from([(
                "document".into(),
                json!({"type":"object","additionalProperties":false,"required":["text"],
                    "properties":{"text":{"type":"string"}}}),
            )]),
        )
        .unwrap();
        let invocation = TaskInvocationV1 {
            plan_id: cas.put(b"exact captured plan").unwrap(),
            node: "root.worker".into(),
            inputs: BTreeMap::new(),
        };
        let context_id = contract
            .prepare(&cas, &invocation, &[], "Only declared context")
            .unwrap();
        let (_, input) = contract.read_context(&cas, &context_id).unwrap();
        Self {
            directory,
            cas,
            contract,
            context_id,
            input,
        }
    }
}

struct NativeModel<'a> {
    fixture: &'a Fixture,
    calls: AtomicUsize,
    /// The address of each cancellation flag the adapter received, in call order.
    cancellations: Mutex<Vec<Option<usize>>>,
    reply: &'a [u8],
    failed: bool,
    usage: u128,
}

impl<'a> NativeModel<'a> {
    fn new(fixture: &'a Fixture, reply: &'a [u8], failed: bool, usage: u128) -> Self {
        Self {
            fixture,
            calls: AtomicUsize::new(0),
            cancellations: Mutex::new(vec![]),
            reply,
            failed,
            usage,
        }
    }
}

impl WorkerModelAdapter for NativeModel<'_> {
    fn provider_kind(&self) -> &'static str {
        "fixture"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("fixture-1".into(), "high".into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.cancellations
            .lock()
            .unwrap()
            .push(cancellation.map(|flag| std::ptr::from_ref(flag) as usize));
        assert!(
            environment.is_empty(),
            "a model Worker receives no sandbox-local environment"
        );
        assert!(std::ptr::eq(cas, &self.fixture.cas));
        assert_eq!(workdir, self.fixture.directory.path());
        assert_eq!(input, self.fixture.input);
        assert_eq!(timeout, TIMEOUT);
        assert_eq!(access, WorkerAccess::ExecuteChecks);
        ModelWorkerReturn {
            usage_observation: None,
            message: if self.failed {
                Err("reported Provider failure".into())
            } else {
                Ok(self.reply.to_vec())
            },
            usage: Some(TaskTokenUsageV3::charge_only(self.usage)),
            raw_artifact_ids: vec![cas.put(RAW).unwrap()],
        }
    }
}

#[test]
fn native_transport_forwards_exact_invocation_and_retains_success_or_failed_evidence() {
    let fixture = Fixture::new();
    let flag = AtomicBool::new(false);
    for failed in [false, true] {
        let model = NativeModel::new(&fixture, VALID_REPLY, failed, u64::MAX.into());
        assert_eq!(model.credential_mode(), CredentialModeV1::TrustedUnsafe);
        for cancellation in [None, Some(&flag)] {
            let result = invoke_model(
                &fixture.cas,
                fixture.directory.path(),
                &model,
                &fixture.contract,
                &fixture.context_id,
                TIMEOUT,
                WorkerAccess::ExecuteChecks,
                cancellation,
            );
            assert_eq!(result.reply.is_err(), failed);
            assert_eq!(
                result.feedback_code,
                failed.then_some(TaskFeedbackCodeV1::ProviderFailure)
            );
            assert_eq!(
                result.usage.unwrap().chargeable_tokens.get(),
                u128::from(u64::MAX)
            );
            assert_eq!(result.raw_artifact_ids.len(), 1);
            assert_eq!(fixture.cas.get(&result.raw_artifact_ids[0]).unwrap(), RAW);
        }
        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            *model.cancellations.lock().unwrap(),
            [None, Some(std::ptr::from_ref(&flag) as usize)],
            "the adapter receives exactly the host's cancellation"
        );
    }
}

#[test]
fn context_and_output_checks_run_around_the_adapter_and_retain_failed_usage() {
    let fixture = Fixture::new();
    for (reply, failed, expected_feedback) in [
        (VALID_REPLY, false, None),
        (
            br#"{"schema":"af.worker-reply/1","outputs":{"document":[{"text":42}]}}"#.as_slice(),
            false,
            Some(TaskFeedbackCodeV1::InvalidOutputContract),
        ),
        (VALID_REPLY, true, Some(TaskFeedbackCodeV1::ProviderFailure)),
    ] {
        let model = NativeModel::new(&fixture, reply, failed, 3);
        let wrong_context = fixture.cas.put(b"not a captured context").unwrap();
        let rejected = invoke_model(
            &fixture.cas,
            fixture.directory.path(),
            &model,
            &fixture.contract,
            &wrong_context,
            TIMEOUT,
            WorkerAccess::ExecuteChecks,
            None,
        );
        assert!(rejected.reply.is_err());
        assert_eq!(
            rejected.feedback_code,
            Some(TaskFeedbackCodeV1::ContextRejected)
        );
        assert_eq!(rejected.usage.unwrap().chargeable_tokens.get(), 0);
        assert!(rejected.raw_artifact_ids.is_empty());
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        let result = invoke_model(
            &fixture.cas,
            fixture.directory.path(),
            &model,
            &fixture.contract,
            &fixture.context_id,
            TIMEOUT,
            WorkerAccess::ExecuteChecks,
            None,
        );
        assert_eq!(result.reply.is_err(), expected_feedback.is_some());
        assert_eq!(result.feedback_code, expected_feedback);
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 3);
        assert_eq!(result.raw_artifact_ids.len(), 1);
        assert_eq!(fixture.cas.get(&result.raw_artifact_ids[0]).unwrap(), RAW);
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    }
}
