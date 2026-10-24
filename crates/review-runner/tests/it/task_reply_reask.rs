//! ADR-0202, issue #283: a typed reply that does not parse is read tolerantly, then re-asked once
//! from the same Worker within the same Attempt, with the parse error, its line and column and
//! the offending excerpt as one more feedback value; both raw replies are kept as CAS blobs and
//! named in the diagnostic. The adapter is in-process and scripted; no Provider runs.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use review_core::task::execution::TaskInvocationV1;
use review_core::task::feedback::TaskFeedbackCodeV1;
use review_core::task::provider_event::ProviderEventV1;
use review_core::task::usage::TaskTokenUsageV3;
use review_runner::task::{
    ModelWorkerReturn, WorkerAccess, WorkerContract, WorkerModelAdapter, invoke_model,
};
use review_store::Cas;
use serde_json::{Value, json};

const VALID: &str =
    r#"{"schema":"af.worker-reply/1","outputs":{"result":[{"verdict":"pass","notes":["a","b"]}]}}"#;
/// The failure the runs recorded: a reviewer reply cut between two array items.
const BROKEN: &str =
    r#"{"schema":"af.worker-reply/1","outputs":{"result":[{"verdict":"pass","notes":["a" "b"]}]}}"#;

/// Answers each call with the next scripted reply and records every request it was sent.
struct Scripted {
    replies: Mutex<Vec<&'static str>>,
    requests: Mutex<Vec<Value>>,
}

impl Scripted {
    fn new(replies: &[&'static str]) -> Self {
        Self {
            replies: Mutex::new(replies.iter().rev().copied().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl WorkerModelAdapter for Scripted {
    fn provider_kind(&self) -> &'static str {
        "fixture"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("fixture-1".into(), "high".into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        _: &Path,
        input: Vec<u8>,
        _: Duration,
        _: WorkerAccess,
        _: Option<&AtomicBool>,
        _: &[(String, String)],
    ) -> ModelWorkerReturn {
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&input).unwrap());
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop()
            .expect("a scripted reply");
        ModelWorkerReturn {
            notes: Default::default(),
            usage_observation: None,
            message: Ok(reply.as_bytes().to_vec()),
            usage: Some(TaskTokenUsageV3::charge_only(10)),
            raw_artifact_ids: vec![cas.put(format!("capture of {reply}").as_bytes()).unwrap()],
            native_failure: None,
        }
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    cas: Cas,
    contract: WorkerContract,
    context_id: String,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let contract = WorkerContract::capture(
        &cas,
        json!({"type":"object"}),
        BTreeMap::from([(
            "result".into(),
            json!({"type":"object","additionalProperties":false,"required":["verdict"],
                "properties":{"verdict":{"enum":["pass","fail"]},
                    "notes":{"type":"array","items":{"type":"string"}}}}),
        )]),
    )
    .unwrap();
    let invocation = TaskInvocationV1 {
        plan_id: cas.put(b"plan").unwrap(),
        node: "root.nodes.review.nodes.bugs".into(),
        inputs: BTreeMap::new(),
    };
    let context_id = contract
        .prepare(&cas, &invocation, &[], "Review it.")
        .unwrap();
    Fixture {
        _directory: directory,
        cas,
        contract,
        context_id,
    }
}

fn run(f: &Fixture, model: &Scripted, reserved: u64) -> review_runner::task::WorkerReturn {
    invoke_model(
        &f.cas,
        Path::new("/nonexistent"),
        model,
        &f.contract,
        &f.context_id,
        Duration::from_secs(60),
        WorkerAccess::ReadOnly,
        None,
        reserved,
    )
}

#[test]
fn a_reply_that_does_not_parse_is_reasked_once_and_the_second_reply_is_admitted() {
    let f = fixture();
    let model = Scripted::new(&[BROKEN, VALID]);
    let returned = run(&f, &model, 1000);
    let reply = returned.reply.expect("the re-asked reply is admitted");
    assert_eq!(reply.outputs["result"][0]["verdict"], "pass");
    assert_eq!(returned.feedback_code, None);
    // One Attempt, two calls: their usage adds.
    assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 20);
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["feedback"].as_array().unwrap().is_empty());
    let reask = &requests[1]["feedback"][0];
    assert_eq!(reask["artifact_type"], "af/WorkerReplyReask@1");
    let payload = &reask["payload"];
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .starts_with("Invalid typed Worker reply: expected `,` or `]` at line 1 column"),
        "{payload}"
    );
    assert_eq!(payload["line"], 1);
    assert!(payload["column"].as_u64().unwrap() > 60);
    assert!(
        payload["excerpt"]
            .as_str()
            .unwrap()
            .contains(r#"["a" "b"]"#),
        "{payload}"
    );
    // The broken reply is kept whole, and so is the exact re-ask request.
    let raw_reply_id = payload["raw_reply_id"].as_str().unwrap();
    assert_eq!(f.cas.get(raw_reply_id).unwrap(), BROKEN.as_bytes());
    assert!(
        returned
            .raw_artifact_ids
            .iter()
            .any(|id| id == raw_reply_id)
    );
    let [
        ProviderEventV1::ReplyReasked {
            raw_reply_id: recorded,
            reask_id,
        },
    ] = returned.notes.events.as_slice()
    else {
        panic!("{:?}", returned.notes.events);
    };
    assert_eq!(recorded, raw_reply_id);
    assert_eq!(
        serde_json::from_slice::<Value>(&f.cas.get(reask_id).unwrap()).unwrap(),
        requests[1]
    );
}

#[test]
fn a_second_reply_that_does_not_parse_fails_and_names_both_kept_replies() {
    let f = fixture();
    // Well-formed this time, but outside the contract: a verdict the schema does not name.
    let second = r#"{"schema":"af.worker-reply/1","outputs":{"result":[{"verdict":"maybe"}]}}"#;
    let model = Scripted::new(&[BROKEN, second]);
    let returned = run(&f, &model, 1000);
    let error = returned.reply.unwrap_err();
    assert_eq!(
        returned.feedback_code,
        Some(TaskFeedbackCodeV1::InvalidOutputContract)
    );
    let first_id = f.cas.put(BROKEN.as_bytes()).unwrap();
    let second_id = f.cas.put(second.as_bytes()).unwrap();
    assert!(error.starts_with("Invalid typed Worker reply: "), "{error}");
    assert!(
        error.contains(&format!(
            "raw reply kept: {first_id}; re-asked once in the same Attempt"
        )),
        "{error}"
    );
    assert!(
        error.ends_with(&format!("raw reply kept: {second_id}")),
        "{error}"
    );
    for id in [&first_id, &second_id] {
        assert!(returned.raw_artifact_ids.contains(id), "{id} not kept");
    }
    assert_eq!(
        model.requests.lock().unwrap().len(),
        2,
        "re-asked once only"
    );
}

#[test]
fn a_valid_object_inside_prose_is_admitted_without_a_reask() {
    let f = fixture();
    let prose: &'static str = Box::leak(
        format!("Here is my review.\n\n{VALID}\n\nLet me know if anything is unclear.")
            .into_boxed_str(),
    );
    let model = Scripted::new(&[prose]);
    let returned = run(&f, &model, 1000);
    assert_eq!(
        returned.reply.unwrap().outputs["result"][0]["notes"],
        json!(["a", "b"])
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert!(returned.notes.events.is_empty());
}

#[test]
fn a_spent_reservation_is_not_reasked() {
    let f = fixture();
    let model = Scripted::new(&[BROKEN]);
    let returned = run(&f, &model, 10);
    let error = returned.reply.unwrap_err();
    assert!(
        error.ends_with("not re-asked: the Attempt's token reservation is spent"),
        "{error}"
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}
