//! ADR-0196 through the common runtime: what one Attempt does about its Provider without failing.
//! A transient failure is called again inside the same Attempt, each wait recorded as a
//! `TaskProviderEvent@1` before it begins; a typed reply that does not parse is re-asked once; a
//! refused admission probe is sent once more in its alternate wording. Every call's usage adds to
//! the one Attempt, and nothing resets its reservation or its deadline.
use super::*;
use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::provider_event::{ProviderEventV1, TransientClassV1};
use review_core::task::usage::TaskTokenUsageV3;
use review_runner::native_failure::{NativeFailureKind, Transient};
use review_runner::task::provider::{PROBE_INPUT, PROBE_INPUT_ALTERNATE};
use review_runner::task::{CallNotes, ModelWorkerReturn, WorkerModelAdapter};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const VALID: &str = r#"{"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed","text":"A checked migration guide"}]}}"#;

/// One scripted answer of the fake Provider.
#[derive(Clone)]
enum Answer {
    /// A reply with this text.
    Reply(&'static str),
    /// A transient failure of this class, asking for a 5 ms wait.
    Transient(TransientClassV1),
    /// A safety-classifier refusal naming this category.
    Refused(&'static str),
}

/// Answers the admission probe with `admission` (then `OK` for its alternate), and each business
/// call with the next scripted answer, the last one repeated.
struct Scripted {
    admission: Answer,
    business: Mutex<Vec<Answer>>,
    calls: AtomicUsize,
    inputs: Mutex<Vec<Vec<u8>>>,
}

impl Scripted {
    fn new(admission: Answer, business: &[Answer]) -> Self {
        Self {
            admission,
            business: Mutex::new(business.iter().rev().cloned().collect()),
            calls: AtomicUsize::new(0),
            inputs: Mutex::new(Vec::new()),
        }
    }
}

impl WorkerModelAdapter for Scripted {
    fn provider_kind(&self) -> &'static str {
        "fixture"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("typed-model".into(), "high".into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        _: &std::path::Path,
        input: Vec<u8>,
        _: std::time::Duration,
        _: review_runner::task::WorkerAccess,
        _: Option<&AtomicBool>,
        _: &[(String, String)],
    ) -> ModelWorkerReturn {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().unwrap().push(input.clone());
        let answer = if input == PROBE_INPUT {
            self.admission.clone()
        } else if input == PROBE_INPUT_ALTERNATE {
            Answer::Reply("OK")
        } else {
            let mut business = self.business.lock().unwrap();
            if business.len() > 1 {
                business.pop().unwrap()
            } else {
                business[0].clone()
            }
        };
        let raw = vec![
            cas.put(format!("native capture {call}").as_bytes())
                .unwrap(),
        ];
        let (message, native_failure, notes) = match answer {
            Answer::Reply(text) => (Ok(text.as_bytes().to_vec()), None, CallNotes::default()),
            Answer::Transient(class) => (
                Err(format!("Claude Worker failed: a fixture failure ({})", class.as_str())),
                match class {
                    TransientClassV1::Network => Some(NativeFailureKind::Network),
                    TransientClassV1::Capacity => Some(NativeFailureKind::Capacity),
                    _ => None,
                },
                CallNotes {
                    transient: Some(Transient {
                        class,
                        retry_after: Some(std::time::Duration::from_millis(5)),
                    }),
                    ..CallNotes::default()
                },
            ),
            Answer::Refused(category) => (
                Err("Claude Worker failed: the bound model's safety classifier flagged the request (model_refused)".into()),
                Some(NativeFailureKind::ModelRefused),
                CallNotes {
                    refusal_category: Some(category.into()),
                    ..CallNotes::default()
                },
            ),
        };
        ModelWorkerReturn {
            notes,
            usage_observation: None,
            message,
            usage: Some(TaskTokenUsageV3::charge_only(7)),
            raw_artifact_ids: raw,
            native_failure,
        }
    }
}

/// What one run of the configured fixture against `model` left in the Store.
struct Ran {
    f: Fixture,
    report: review_graph::RunReport,
}

impl Ran {
    fn projection(&mut self) -> review_store::store::task::TaskProjection {
        self.f.store = EventStore::open(self.f._directory.path().join("events.sqlite")).unwrap();
        self.f
            .store
            .task_projection(&self.f.cas, &self.f.task.task_id)
            .unwrap()
            .unwrap()
    }

    /// Every settled Attempt of `node`: its charge, its raw evidence and, when it failed, its
    /// diagnostic.
    fn settled(&mut self, node: &str) -> Vec<(u128, Vec<String>, Option<String>)> {
        let projection = self.projection();
        let execution = projection.execution.unwrap();
        execution
            .attempt_accounting()
            .into_iter()
            .filter(|row| row.reservation.node == node && row.settled_unix_ms.is_some())
            .map(|row| {
                let diagnostic = match row.result {
                    Some(TaskAttemptResultV1::Failed { diagnostic_id, .. }) => Some(
                        self.f.cas.get_json(&diagnostic_id).unwrap()["error"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                    ),
                    _ => None,
                };
                (row.charged_tokens, row.raw_artifact_ids, diagnostic)
            })
            .collect()
    }
}

fn run(model: &Scripted) -> Ran {
    let mut f = super::wide_usage::configured_fixture();
    let report = {
        let models = f
            .plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: model as &dyn WorkerModelAdapter,
                    },
                )
            })
            .collect();
        let domain = review_pipeline::task::provider::ProviderTaskDomain {
            graph: &f.graph,
            models: &models,
            inner: &DocumentDomain,
        };
        let host = CapturedTaskHost::capture_with_models(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &domain,
            &models,
        )
        .unwrap();
        let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "writer", 60000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
        runtime.execute().unwrap()
    };
    Ran { f, report }
}

/// Issue #295: each transient class, failing twice, is waited out twice within the one Attempt,
/// each wait recorded before it begins; the third call's reply completes the node, and the
/// Attempt is charged all three calls.
#[test]
fn each_transient_class_failing_twice_is_retried_within_one_attempt() {
    for class in TransientClassV1::ALL {
        let model = Scripted::new(
            Answer::Reply("OK"),
            &[
                Answer::Transient(class),
                Answer::Transient(class),
                Answer::Reply(VALID),
            ],
        );
        let mut ran = run(&model);
        assert!(ran.report.complete(), "{class:?}: {:?}", ran.report);
        let settled = ran.settled("root.nodes.write");
        assert_eq!(settled.len(), 1, "{class:?}: one Attempt");
        let (charged, raw, diagnostic) = &settled[0];
        assert_eq!(*charged, 21, "{class:?}: three calls of 7");
        assert_eq!(raw.len(), 3, "{class:?}: every call's capture kept");
        assert!(diagnostic.is_none());
        let waits: Vec<_> = ran
            .projection()
            .provider_events
            .into_iter()
            .map(|event| (event.node, event.event))
            .collect();
        assert_eq!(
            waits,
            [1, 2].map(|retry| (
                "root.nodes.write".to_string(),
                ProviderEventV1::RetryWait {
                    class,
                    retry,
                    max_retries: 3,
                    wait_ms: 5,
                    retry_after: true,
                }
            )),
            "{class:?}"
        );
        // The admission probe and three business calls.
        assert_eq!(model.calls.load(Ordering::SeqCst), 4, "{class:?}");
    }
}

/// A transient failure that outlasts three retries fails the Attempt with its class and the
/// retry history in its diagnostic.
#[test]
fn an_exhausted_transient_failure_names_its_class_and_retry_history() {
    let model = Scripted::new(
        Answer::Reply("OK"),
        &[Answer::Transient(TransientClassV1::Capacity)],
    );
    let mut ran = run(&model);
    assert!(!ran.report.complete());
    let settled = ran.settled("root.nodes.write");
    assert!(!settled.is_empty());
    let (charged, _, diagnostic) = &settled[0];
    assert_eq!(*charged, 28, "one call and three retries, all charged");
    let diagnostic = diagnostic.as_deref().unwrap();
    assert!(
        diagnostic.starts_with("Claude Worker failed: a fixture failure (capacity)"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(
            "; transient Provider failure (capacity) after 3 of 3 retries (retry 1 after \
             capacity waited 5 ms, retry 2 after capacity waited 5 ms, retry 3 after capacity \
             waited 5 ms); not retried again: its 3 retries are spent"
        ),
        "{diagnostic}"
    );
}

/// Issue #283: a reply that does not parse is re-asked once in the same Attempt; the second reply
/// completes the node. The broken reply is kept as raw evidence and the re-ask is a recorded
/// Provider event of that Attempt.
#[test]
fn a_reply_that_does_not_parse_once_is_reasked_and_completes_the_node() {
    let model = Scripted::new(
        Answer::Reply("OK"),
        &[
            Answer::Reply(
                r#"{"schema":"af.worker-reply/1","outputs":{"output":[{"outcome":"passed" "text":"x"}]}}"#,
            ),
            Answer::Reply(VALID),
        ],
    );
    let mut ran = run(&model);
    assert!(ran.report.complete(), "{:?}", ran.report);
    let settled = ran.settled("root.nodes.write");
    assert_eq!(settled.len(), 1);
    let (charged, raw, _) = &settled[0];
    assert_eq!(*charged, 14);
    let events = ran.projection().provider_events;
    let [event] = events.as_slice() else {
        panic!("{events:?}");
    };
    let ProviderEventV1::ReplyReasked {
        raw_reply_id,
        reask_id,
    } = &event.event
    else {
        panic!("{event:?}");
    };
    assert!(
        raw.contains(raw_reply_id) && raw.contains(reask_id),
        "{raw:?}"
    );
    assert!(
        String::from_utf8(ran.f.cas.get(raw_reply_id).unwrap())
            .unwrap()
            .contains(r#""outcome":"passed" "text""#)
    );
}

/// A reply that does not parse twice fails the Attempt, its diagnostic naming both kept replies;
/// the node's next Attempt gets typed retry feedback, never the reply itself.
#[test]
fn a_reply_that_does_not_parse_twice_fails_the_attempt_and_names_both_kept_replies() {
    let model = Scripted::new(Answer::Reply("OK"), &[Answer::Reply("not json at all")]);
    let mut ran = run(&model);
    assert!(!ran.report.complete());
    let settled = ran.settled("root.nodes.write");
    let (charged, raw, diagnostic) = &settled[0];
    assert_eq!(*charged, 14, "the reply and its re-ask");
    let kept = ran.f.cas.put(b"not json at all").unwrap();
    assert!(raw.contains(&kept), "{raw:?}");
    let diagnostic = diagnostic.as_deref().unwrap();
    assert!(
        diagnostic.starts_with("Invalid typed Worker reply: expected ident at line 1 column 2"),
        "{diagnostic}"
    );
    assert_eq!(
        diagnostic
            .matches(&format!("raw reply kept: {kept}"))
            .count(),
        2,
        "{diagnostic}"
    );
    // The failed node's own line names its class and first line; nothing that follows it reads
    // as the cause.
    let failed: Vec<_> = ran
        .report
        .outcomes
        .iter()
        .filter_map(|(node, outcome)| match outcome {
            review_graph::NodeOutcome::Failed { error, .. } => Some((node.clone(), error.clone())),
            _ => None,
        })
        .collect();
    assert!(
        failed.iter().any(|(node, _)| node == "root.nodes.write"),
        "{failed:?}"
    );
    for (node, error) in &failed {
        if error.starts_with("Required branch receipt was not produced") {
            assert!(error.contains("this node follows root."), "{node}: {error}");
        }
    }
}

/// ADR-0196: a safety-classifier refusal of the admission probe itself is answered once with the
/// alternate probe in the same Attempt; its `OK` admits the Provider, the business Worker runs,
/// and the refusal's category is the recorded event's.
#[test]
fn a_refused_admission_probe_is_sent_once_more_in_its_alternate_wording() {
    let model = Scripted::new(Answer::Refused("cyber"), &[Answer::Reply(VALID)]);
    let mut ran = run(&model);
    assert!(ran.report.complete(), "{:?}", ran.report);
    let inputs = model.inputs.lock().unwrap().clone();
    assert_eq!(inputs[0], PROBE_INPUT);
    assert_eq!(inputs[1], PROBE_INPUT_ALTERNATE);
    let projection = ran.projection();
    let [event] = projection.provider_events.as_slice() else {
        panic!("{:?}", projection.provider_events);
    };
    assert!(event.node.starts_with("root.providers.admit"), "{event:?}");
    assert_eq!(
        event.event,
        ProviderEventV1::AdmissionReprobed {
            category: Some("cyber".into())
        }
    );
    let admission = projection
        .execution
        .unwrap()
        .attempt_accounting()
        .into_iter()
        .find(|row| row.reservation.node == event.node)
        .unwrap();
    assert_eq!(admission.charged_tokens, 14, "both probes are charged");
    let alternate = ran.f.cas.put(PROBE_INPUT_ALTERNATE).unwrap();
    assert!(admission.raw_artifact_ids.contains(&alternate));
}

/// A second refusal of the alternate probe fails the admission with both refusals named, and no
/// business Worker starts.
#[test]
fn a_second_refusal_fails_the_admission_and_starts_no_worker() {
    struct Refusing(AtomicUsize);
    impl WorkerModelAdapter for Refusing {
        fn provider_kind(&self) -> &'static str {
            "fixture"
        }
        fn model_settings(&self) -> Option<(String, String)> {
            Some(("typed-model".into(), "high".into()))
        }
        fn invoke(
            &self,
            cas: &Cas,
            _: &std::path::Path,
            _: Vec<u8>,
            _: std::time::Duration,
            _: review_runner::task::WorkerAccess,
            _: Option<&AtomicBool>,
            _: &[(String, String)],
        ) -> ModelWorkerReturn {
            let call = self.0.fetch_add(1, Ordering::SeqCst);
            ModelWorkerReturn {
                notes: CallNotes::default(),
                usage_observation: None,
                message: Err(format!("refusal {call}")),
                usage: Some(TaskTokenUsageV3::charge_only(3)),
                raw_artifact_ids: vec![cas.put(format!("refusal {call}").as_bytes()).unwrap()],
                native_failure: Some(NativeFailureKind::ModelRefused),
            }
        }
    }
    let model = Refusing(AtomicUsize::new(0));
    let mut f = super::wide_usage::configured_fixture();
    let models = f
        .plan
        .bindings
        .iter()
        .map(|(slot, binding)| {
            (
                slot.clone(),
                TaskModelBinding {
                    binding: binding.clone(),
                    adapter: &model as &dyn WorkerModelAdapter,
                },
            )
        })
        .collect();
    let domain = review_pipeline::task::provider::ProviderTaskDomain {
        graph: &f.graph,
        models: &models,
        inner: &DocumentDomain,
    };
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &domain,
        &models,
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer", 60000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    let (node, error) = report
        .outcomes
        .iter()
        .find_map(|(node, outcome)| match outcome {
            review_graph::NodeOutcome::Failed { error, .. }
                if node.starts_with("root.providers.") =>
            {
                Some((node.clone(), error.clone()))
            }
            _ => None,
        })
        .unwrap();
    assert!(
        error.starts_with(
            "refusal 0; the alternate admission probe was sent once and failed too: refusal 1"
        ),
        "{node}: {error}"
    );
    assert_eq!(
        model.0.load(Ordering::SeqCst),
        2,
        "no business Worker started"
    );
    // The Worker waited for the admission and never started.
    assert!(
        matches!(
            report.outcome("root.nodes.write"),
            Some(review_graph::NodeOutcome::Suppressed { .. })
        ),
        "{report:?}"
    );
}
