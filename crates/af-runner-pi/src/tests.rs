use super::*;
use std::os::unix::fs::PermissionsExt;

fn ready() -> Ready {
    Ready {
        catalog_id: digest(b"catalog"),
        principal: Some(digest(b"account")),
        auth_method: "oauth".into(),
        next_offset: None,
        backend: "openai-codex".into(),
        version: SDK_VERSION.into(),
        models: vec![Model {
            id: "o3".into(),
            efforts: vec!["high".into()],
            identity: digest(b"model"),
        }],
    }
}
fn record(state: &mut State, kind: &str, data: Value) -> Result<(), String> {
    let mut value = data;
    if kind == "usage" && value.get("observations").is_none() {
        value["observations"] = json!(1);
    }
    value["schema"] = json!(PROTOCOL);
    value["type"] = json!(kind);
    state.record(&serde_json::to_vec(&value).unwrap())
}
fn accepted() -> State {
    let mut state = State::default();
    record(&mut state, "ready", serde_json::to_value(ready()).unwrap()).unwrap();
    record(
        &mut state,
        "prepared",
        json!({"model":"o3","effort":"high","model_identity":digest(b"model")}),
    )
    .unwrap();
    state.prompt_sent = true;
    state.prompt_nonce = Some(digest(b"prompt challenge"));
    record(
        &mut state,
        "accepted",
        json!({"prompt_nonce":digest(b"prompt challenge")}),
    )
    .unwrap();
    state
}
fn usage(id: u64, complete: bool, input: Value, output: Value) -> Value {
    json!({"inference":id,"complete":complete,"input":input,"output":output,"cache_read":3,"cache_write":null,"reasoning":null})
}
#[test]
fn cumulative_usage_replaces_snapshots_and_final_echoes_count_once() {
    let mut state = accepted();
    record(&mut state, "usage", usage(1, false, json!(10), json!(2))).unwrap();
    record(&mut state, "usage", usage(1, true, json!(12), json!(4))).unwrap();
    record(&mut state, "usage", usage(1, true, json!(12), json!(4))).unwrap();
    record(&mut state, "usage", usage(2, true, json!(9), json!(2))).unwrap();
    record(&mut state, "settled", json!({})).unwrap();
    let observation = state.observation();
    assert!(observation.charge_complete);
    let usage = observation.reported_usage.unwrap();
    assert_eq!(usage.input_tokens.unwrap().get(), 21);
    assert_eq!(usage.cache_read_tokens.unwrap().get(), 6);
    assert_eq!(usage.chargeable_tokens.get(), 21);
    assert!(usage.cache_write_tokens.is_none());
    assert!(usage.reasoning_tokens.is_none());
}
#[test]
fn anthropic_normalized_optional_caches_settle_with_exact_total_charge() {
    for (read, write) in [(0, 0), (3, 0), (0, 2), (3, 2)] {
        let mut state = accepted();
        state.ready.as_mut().unwrap().backend = "anthropic".into();
        for (input, output, complete) in [(7, 0, false), (8, 3, true)] {
            record(
                &mut state,
                "usage",
                json!({"inference":1,"complete":complete,"observations":2,
                    "input":input + read + write,"output":output,
                    "cache_read":read,"cache_write":write,"reasoning":null}),
            )
            .unwrap();
        }
        record(&mut state, "settled", json!({})).unwrap();
        let observed = state.observation();
        assert!(observed.charge_complete);
        let usage = observed.reported_usage.unwrap();
        assert_eq!(usage.input_tokens.unwrap().get(), 8 + read + write);
        assert_eq!(usage.output_tokens.unwrap().get(), 3);
        assert_eq!(usage.cache_read_tokens.unwrap().get(), read);
        assert_eq!(usage.cache_write_tokens.unwrap().get(), write);
        assert_eq!(usage.chargeable_tokens.get(), 11 + read + write);
    }
}

#[test]
fn errors_unknown_and_partial_usage_retain_the_observed_floor() {
    let mut state = accepted();
    assert!(state.observation().reported_usage.is_none());
    let mut unknown = usage(1, false, Value::Null, Value::Null);
    unknown["cache_read"] = Value::Null;
    record(&mut state, "usage", unknown).unwrap();
    assert!(
        state.observation().reported_usage.is_none(),
        "SDK-initialized zero is not a report"
    );

    record(&mut state, "usage", usage(1, false, Value::Null, json!(7))).unwrap();
    record(&mut state, "error", json!({"code":"inference_failed"})).unwrap();
    let observation = state.observation();
    assert!(!observation.charge_complete);
    let usage = observation.reported_usage.unwrap();
    assert_eq!(usage.chargeable_tokens.get(), 7);
    assert!(usage.input_tokens.is_none());
    assert_eq!(usage.output_tokens.unwrap().get(), 7);
}
#[test]
fn prompt_acknowledgement_requires_delivery_and_exact_fresh_challenge() {
    let accepted_state = accepted();
    for (sent, nonce) in [
        (false, digest(b"prompt challenge")),
        (true, digest(b"wrong")),
    ] {
        let mut state = accepted();
        state.accepted = false;
        state.prompt_sent = sent;
        assert!(record(&mut state, "accepted", json!({"prompt_nonce":nonce})).is_err());
    }
    for bad in [
        json!({}),
        json!({"prompt_nonce":accepted_state.prompt_nonce, "extra":true}),
    ] {
        let mut state = accepted();
        state.accepted = false;
        assert!(record(&mut state, "accepted", bad).is_err());
    }
    let mut state = accepted();
    state.accepted = false;
    assert!(state.record(json!({"schema":"af.pi-host/1","type":"accepted","prompt_nonce":state.prompt_nonce}).to_string().as_bytes()).is_err());
}

#[test]
fn invalid_counters_versions_and_lifecycle_fail_closed() {
    for bad in [
        json!({"schema":"af.pi-host/0","type":"settled"}),
        json!({"schema":PROTOCOL,"type":"accepted"}),
    ] {
        assert!(State::default().record(bad.to_string().as_bytes()).is_err());
    }
    let mut state = accepted();
    record(&mut state, "usage", usage(1, true, json!(10), json!(2))).unwrap();
    assert!(record(&mut state, "usage", usage(1, true, json!(11), json!(2))).is_err());
    assert!(record(&mut accepted(), "usage", usage(1, true, json!(1), json!(2))).is_err());
    assert!(
        record(
            &mut accepted(),
            "usage",
            usage(0, true, json!(10), json!(2))
        )
        .is_err()
    );
    assert!(record(&mut accepted(), "error", json!({"code":"raw secret"})).is_err());
    for write in [8, 11] {
        let mut impossible = usage(1, true, json!(10), json!(2));
        impossible["cache_write"] = json!(write);
        assert!(record(&mut accepted(), "usage", impossible).is_err());
    }
    let mut missing = usage(1, true, json!(10), json!(2));
    missing.as_object_mut().unwrap().remove("input");
    assert!(record(&mut accepted(), "usage", missing).is_err());
    assert!(
        record(
            &mut accepted(),
            "usage",
            usage(1, true, json!(u64::MAX), json!(2))
        )
        .is_err()
    );
    record(&mut state, "settled", json!({})).unwrap();
    assert!(record(&mut state, "error", json!({"code":"host_refused"})).is_err());
}

#[test]
fn kimi_invalid_missing_conflicting_and_unsafe_counters_keep_prior_observed_floor() {
    for field in ["input", "output", "cache_read", "cache_write", "reasoning"] {
        for bad in [
            json!(-1),
            json!(0.5),
            json!("38"),
            json!(true),
            json!(9_007_199_254_740_992u64),
            json!(u64::MAX),
        ] {
            let mut state = accepted();
            let identity = state.ready.as_mut().unwrap();
            identity.backend = "kimi-coding".into();
            identity.principal = None;
            identity.models[0].id = "k3".into();
            identity.validate().unwrap();
            let prior = json!({"inference":1,"complete":false,"input":439,"output":38,
                "cache_read":null,"cache_write":null,"reasoning":24});
            record(&mut state, "usage", prior.clone()).unwrap();
            let mut next = prior.clone();
            next[field] = bad;
            assert!(record(&mut state, "usage", next).is_err(), "{field}");
            let mut missing = prior;
            missing.as_object_mut().unwrap().remove(field);
            assert!(record(&mut state, "usage", missing).is_err(), "{field}");
            let observation = state.observation();
            assert!(!observation.charge_complete);
            let observed = observation.reported_usage.unwrap();
            assert_eq!(observed.chargeable_tokens.get(), 477);
            assert_eq!(observed.input_tokens.unwrap().get(), 439);
            assert_eq!(observed.output_tokens.unwrap().get(), 38);
            assert_eq!(observed.reasoning_tokens.unwrap().get(), 24);
            assert!(observed.cache_read_tokens.is_none());
            assert!(observed.cache_write_tokens.is_none());
        }
    }
}

mod process {
    use super::*;
    fn fixture(mode: &str, root: &Path, socket: Option<&Path>) -> PiTaskAdapter {
        let node = root.join("node");
        let mut identity = ready();
        if mode.starts_with("kimi-") {
            identity.backend = "kimi-coding".into();
            identity.principal = None;
            identity.models[0].id = "k3".into();
        }
        if mode.starts_with("pages-") {
            identity.backend = "team-compatible".into();
            identity.auth_method = "api_key".into();
            identity.principal = None;
            identity.models[0].id = "~vendor/model-69".into();
        }
        let mut response = serde_json::to_value(&identity).unwrap();
        response["schema"] = json!(PROTOCOL);
        response["type"] = json!("ready");
        let body = r#"#!/usr/bin/python3 -B
import json,os,sys,socket,signal
mode=MODE
open(START_MARKER,'w').write('started')
ready=json.loads(READY)
def emit(v):
 if v.get('type')=='usage': v.setdefault('observations',1)
 v['schema']='af.pi-host/5'
 data=(json.dumps(v,ensure_ascii=False)+'\n').encode()
 # Split inside a UTF-8 sequence too: LF is the only record separator.
 if mode.startswith('pages-'): os.write(1,data); return
 for at in range(0,len(data),len(data) if mode=='immediate' else 7): os.write(1,data[at:at+7] if mode!='immediate' else data)
r=json.loads(sys.stdin.readline())
assert 'NODE_OPTIONS' not in os.environ and 'OPENAI_API_KEY' not in os.environ
if mode=='readonly': assert not r['writable']
if mode=='writable': assert r['writable']==[r['cwd']]
if mode=='identity': ready['principal']='sha256:'+'b'*64
if mode=='backend': ready['backend']='anthropic'
if mode=='version': ready['version']='1.5.0'
if mode.startswith('pages-'):
 offset=r.get('catalog_offset',0)
 catalog=[dict(ready['models'][0],id='~vendor/model-'+str(i)) for i in range(70)]
 if r.get('model'): catalog=[m for m in catalog if m['id']==r['model']]
 ready['models']=catalog[offset:offset+64]
 if offset+64<len(catalog):ready['next_offset']=offset+64
 if mode=='pages-offset':ready['next_offset']=63
 if mode=='pages-limit':ready['next_offset']=4096
 if mode=='pages-null':ready['next_offset']=None
 if offset and mode=='pages-duplicate':ready['models'][0]['id']='~vendor/model-0'
 if offset and mode=='pages-catalog':ready['catalog_id']='sha256:'+'c'*64
 if offset and mode=='pages-auth':ready['auth_method']='context'
emit(ready)
if r['probe']: sys.exit(0)
p=json.loads(sys.stdin.readline())
if mode=='model': p['model']='wrong'
if mode=='effort': p['effort']='low'
emit({'type':'prepared','model':p['model'],'effort':p['effort'],'model_identity':p['model_identity']})
if mode=='before-prompt':
 emit({'type':'accepted','prompt_nonce':'sha256:'+'0'*64})
 emit({'type':'usage','inference':1,'complete':True,'input':10,'output':2,'cache_read':3,'cache_write':None,'reasoning':None})
 emit({'type':'message','text':'{"schema":"af.worker-reply/1","outputs":{"report":[{"summary":"premature"}]}}'})
 emit({'type':'settled'})
 prompt=json.loads(sys.stdin.readline())
 open(MARKER,'w').write(prompt['text'])
 sys.exit(0)
prompt=json.loads(sys.stdin.readline())
open(MARKER,'w').write(prompt['text'])
emit({'type':'accepted','prompt_nonce':prompt['prompt_nonce']})
if mode.startswith('kimi-') or mode=='openai-no-cache':
 u={'type':'usage','inference':1,'complete':True,'input':439,'output':38,'cache_read':None,'cache_write':None,'reasoning':24}
 if mode=='kimi-cache': u.update(cache_read=200,cache_write=100)
 if mode=='kimi-no-reasoning': u['reasoning']=None
 if mode=='kimi-missing-input': u['input']=None
 if mode=='kimi-missing-output': u['output']=None
 if mode=='kimi-repeated':
  emit(dict(u,complete=False,input=400,output=30))
 emit(u)
 if mode=='kimi-repeated': emit(u)
 if mode=='kimi-multiple': emit(dict(u,inference=2))
 if mode in ['kimi-malformed','kimi-overflow','kimi-conflict']:
  bad=dict(u)
  if mode=='kimi-malformed': bad['output']='38'
  if mode=='kimi-overflow': bad['input']=2**64
  if mode=='kimi-conflict': bad['cache_read']=440
  emit(bad)
 if mode=='kimi-failed':
  emit({'type':'error','code':'inference_failed'}); sys.exit(0)
 emit({'type':'message','text':'aggregate result'})
 emit({'type':'settled'}); sys.exit(0)
if mode!='no-usage':
 emit({'type':'usage','inference':1,'complete':mode not in ['incomplete','earlier-incomplete'],'input':None if mode=='unknown-usage' else 10,'output':None if mode=='unknown-usage' else 2,'cache_read':None if mode=='unknown-usage' else 3,'cache_write':None,'reasoning':None})
if mode=='earlier-incomplete': emit({'type':'usage','inference':2,'complete':True,'input':10,'output':2,'cache_read':3,'cache_write':None,'reasoning':None})
if mode in ['cancel','cancel-no-pid']:
 s=socket.socket(socket.AF_UNIX); s.connect(SOCKET)
 if mode=='cancel-no-pid': sys.exit(1)
 s.sendall(str(os.getpid()).encode()); s.close()
 # Wait for supervisor cancellation; stdin can already be closed after the prompt.
 signal.pause(); sys.exit(1)
if mode=='oversize': os.write(1,b'x'*(4*1024*1024+1)); sys.exit(0)
if mode=='malformed': os.write(1,b'garbage\n'); sys.exit(0)
if mode=='partial': os.write(1,b'{"schema":'); sys.exit(0)
if mode=='error': emit({'type':'error','code':'inference_failed'}); sys.exit(0)
emit({'type':'message','text':'unicode \u2028 \u2029 result'})
if mode!='eof': emit({'type':'settled'})
if mode=='after': emit({'type':'error','code':'inference_failed'})
"#;
        let body = body
            .replace("START_MARKER", &json!(root.join("started")).to_string())
            .replace("MODE", &json!(mode).to_string())
            .replace("READY", &json!(response.to_string()).to_string())
            .replace("MARKER", &json!(root.join("prompt")).to_string())
            .replace(
                "SOCKET",
                &json!(
                    socket
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )
                .to_string(),
            );
        std::fs::write(&node, body).unwrap();
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = PiConfig {
            backend: identity.backend.clone(),
            sdk_entrypoint: root.join("sdk/dist/index.js"),
        };
        let captured = authority(
            &node,
            &config,
            root,
            &identity.models[0],
            &identity.auth_method,
        )
        .unwrap();
        PiTaskAdapter {
            node,
            config,
            auth_dir: root.into(),
            principal: identity.principal,
            model: identity.models[0].id.clone(),
            effort: "high".into(),
            captured,
        }
    }
    #[test]
    fn missing_trusted_allowance_refuses_before_start_even_with_payload_and_environment_budget() {
        let root = tempfile::tempdir().unwrap();
        let adapter = fixture("readonly", root.path(), None);
        let cas = Cas::open(root.path().join("cas")).unwrap();
        let returned = adapter.invoke(
            &cas,
            root.path(),
            br#"{"remaining_tokens":999999}"#.to_vec(),
            Duration::from_secs(30),
            WorkerAccess::ReadOnly,
            None,
            &[("AF_PI_REMAINING_TOKENS".into(), "999999".into())],
        );
        assert!(
            returned
                .message
                .unwrap_err()
                .contains("trusted remaining token allowance")
        );
        assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 0);
        assert!(!root.path().join("started").exists());
        let invalid = adapter
            .invoke_with_allowance(
                &cas,
                root.path(),
                b"request".to_vec(),
                Duration::from_secs(30),
                WorkerAccess::ReadOnly,
                None,
                &[],
                RemainingTokenAllowance::new(u64::MAX).unwrap(),
                af_runner::task::reply_schema::ReplyContract::Request,
            )
            .unwrap_or_else(|unstarted| *unstarted.0);
        assert!(invalid.message.is_err());
        assert_eq!(invalid.usage.unwrap().chargeable_tokens.get(), 0);
        assert!(!root.path().join("started").exists());
    }

    #[test]
    fn safe_evidence_distinguishes_requests_with_identical_usage_without_sensitive_payloads() {
        let root = tempfile::tempdir().unwrap();
        let adapter = fixture("immediate", root.path(), None);
        let cas = Cas::open(root.path().join("cas")).unwrap();
        let mut evidence = vec![];
        for _ in 0..2 {
            let returned = invoke_test(
                &adapter,
                &cas,
                root.path(),
                b"PRIVATE_PROMPT_SENTINEL".to_vec(),
                Duration::from_secs(30),
                WorkerAccess::ReadOnly,
                None,
                &[],
            );
            assert!(returned.message.is_ok());
            let bytes = cas.get(&returned.raw_artifact_ids[0]).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_PROMPT_SENTINEL"));
            evidence.push(PiInferenceEvidenceV2::decode(&bytes).unwrap());
        }
        assert_ne!(evidence[0].request_nonce, evidence[1].request_nonce);
        assert_eq!(evidence[0].aggregate, evidence[1].aggregate);
        assert_eq!(evidence[0].started_inferences, 1);
    }

    #[test]
    fn kimi_adapter_aggregate_budget_preserves_unknown_cache_and_refuses_bad_observations() {
        for (mode, complete, charge, message_ok) in [
            ("kimi-aggregate", true, 477, true),
            ("kimi-cache", true, 477, true),
            ("kimi-no-reasoning", true, 477, true),
            ("kimi-repeated", true, 477, true),
            ("kimi-multiple", true, 954, true),
            ("kimi-missing-input", false, 38, true),
            ("kimi-missing-output", false, 439, true),
            ("kimi-malformed", false, 477, false),
            ("kimi-overflow", false, 477, false),
            ("kimi-conflict", false, 477, false),
            ("kimi-failed", false, 477, false),
            ("openai-no-cache", false, 38, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture(mode, root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let returned = invoke_test(
                &adapter,
                &cas,
                root.path(),
                b"private request".to_vec(),
                Duration::from_secs(30),
                WorkerAccess::ReadOnly,
                None,
                &[],
            );
            assert_eq!(
                returned.message.is_ok(),
                message_ok,
                "{mode}: {:?}",
                returned.message
            );
            assert_eq!(returned.raw_artifact_ids.len(), 1);
            let bytes = cas.get(&returned.raw_artifact_ids[0]).unwrap();
            PiInferenceEvidenceV2::decode(&bytes).unwrap();
            let evidence: Value = serde_json::from_slice(&bytes).unwrap();
            let mut old = evidence.clone();
            old["schema"] = json!("af/pi-inference-evidence@0");
            assert!(PiInferenceEvidenceV2::decode(&serde_json::to_vec(&old).unwrap()).is_err());
            let mut secret = evidence.clone();
            secret["tool_arguments"] = json!("PRIVATE");
            assert!(PiInferenceEvidenceV2::decode(&serde_json::to_vec(&secret).unwrap()).is_err());
            assert_eq!(evidence["schema"], "af/pi-inference-evidence@2");
            assert_eq!(evidence["remaining_tokens"], 1_000_000);
            assert_eq!(
                evidence["started_inferences"],
                if mode == "kimi-multiple" { 2 } else { 1 }
            );
            assert_eq!(
                evidence["inferences"][0]["input"],
                if mode == "kimi-missing-input" {
                    Value::Null
                } else {
                    json!(439)
                }
            );
            assert!(!evidence.to_string().contains("private request"));
            // Incomplete observations refuse Task admission even when lifecycle/text succeeded.
            assert_eq!(returned.usage_observation.is_none(), complete, "{mode}");
            if let Some(observation) = &returned.usage_observation {
                assert!(!observation.charge_complete, "{mode}");
                assert_eq!(observation.reported_usage, returned.usage, "{mode}");
            }
            let observed = returned.usage.unwrap();
            assert_eq!(observed.chargeable_tokens.get(), charge, "{mode}");
            assert_eq!(
                observed.input_tokens.map(|v| v.get()),
                (mode != "kimi-missing-input").then_some(if mode == "kimi-multiple" {
                    878
                } else {
                    439
                }),
                "{mode}"
            );
            assert_eq!(
                observed.output_tokens.map(|v| v.get()),
                (mode != "kimi-missing-output").then_some(if mode == "kimi-multiple" {
                    76
                } else {
                    38
                }),
                "{mode}"
            );
            assert_eq!(
                observed.cache_read_tokens.map(|v| v.get()),
                (mode == "kimi-cache").then_some(200),
                "{mode}"
            );
            assert_eq!(
                observed.cache_write_tokens.map(|v| v.get()),
                (mode == "kimi-cache").then_some(100),
                "{mode}"
            );
            assert_eq!(
                observed.reasoning_tokens.map(|v| v.get()),
                (mode != "kimi-no-reasoning").then_some(if mode == "kimi-multiple" {
                    48
                } else {
                    24
                }),
                "{mode}"
            );
        }
    }

    #[test]
    fn expired_or_cancelled_attempts_do_not_start_a_host() {
        for expired in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture("success", root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let cancelled = AtomicBool::new(!expired);
            let returned = invoke_test(
                &adapter,
                &cas,
                root.path(),
                b"PRIVATE".to_vec(),
                if expired {
                    Duration::ZERO
                } else {
                    Duration::from_secs(30)
                },
                WorkerAccess::ReadOnly,
                Some(&cancelled),
                &[],
            );
            assert!(returned.message.is_err());
            assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 0);
            assert!(!root.path().join("started").exists());
        }
    }

    #[test]
    fn file_tool_write_grants_follow_source_write_effects_without_shell_authority() {
        for (access, mode) in [
            (WorkerAccess::ReadOnly, "readonly"),
            (WorkerAccess::ExecuteChecks, "readonly"),
            (WorkerAccess::WriteSource, "writable"),
            (WorkerAccess::WriteSourceWithShell, "writable"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture(mode, root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let returned = invoke_test(
                &adapter,
                &cas,
                root.path(),
                b"request".to_vec(),
                Duration::from_secs(30),
                access,
                None,
                &[],
            );
            assert!(returned.message.is_ok(), "{access:?}");
        }
    }

    #[test]
    fn supervised_host_partial_jsonl_unicode_and_immediate_terminal_settlement() {
        for mode in ["success", "immediate"] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture(mode, root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let returned = invoke_test(
                &adapter,
                &cas,
                root.path(),
                b"private request".to_vec(),
                Duration::from_secs(30),
                WorkerAccess::ReadOnly,
                None,
                &[],
            );
            assert_eq!(
                returned.message.unwrap(),
                "unicode \u{2028} \u{2029} result".as_bytes()
            );
            assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 9);
            assert_eq!(
                std::fs::read(root.path().join("prompt")).unwrap(),
                b"private request"
            );
        }
    }
    #[test]
    fn adapter_refuses_a_complete_reply_emitted_before_prompt_consumption() {
        let root = tempfile::tempdir().unwrap();
        let adapter = fixture("before-prompt", root.path(), None);
        let cas = Cas::open(root.path().join("cas")).unwrap();
        let returned = invoke_test(
            &adapter,
            &cas,
            root.path(),
            b"PRIVATE".to_vec(),
            Duration::from_secs(30),
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        let error = returned
            .message
            .expect_err("accepted a complete reply emitted before prompt consumption");
        assert!(
            error == "Pi host lifecycle violation"
                || error == "Pi host acknowledged a prompt it was not delivered",
            "{error}"
        );
    }

    #[test]
    fn settlement_requires_every_observed_inference_complete_without_inventing_usage() {
        for (mode, success, charge) in [
            ("no-usage", false, None),
            ("incomplete", false, Some(9)),
            ("earlier-incomplete", false, Some(18)),
            ("unknown-usage", true, None),
        ] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture(mode, root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let returned = invoke_test(
                &adapter,
                &cas,
                root.path(),
                b"private request".to_vec(),
                Duration::from_secs(30),
                WorkerAccess::ReadOnly,
                None,
                &[],
            );
            assert_eq!(returned.message.is_ok(), success, "{mode}");
            if !success {
                assert_eq!(
                    returned.message.unwrap_err(),
                    "Pi settled without complete inference observations",
                    "{mode}"
                );
            }
            let observation = returned.usage_observation.unwrap();
            assert!(!observation.charge_complete, "{mode}");
            assert_eq!(
                observation
                    .reported_usage
                    .map(|u| u.chargeable_tokens.get()),
                charge,
                "{mode}"
            );
            assert_eq!(
                std::fs::read(root.path().join("prompt")).unwrap(),
                b"private request"
            );
        }
    }

    #[test]
    fn observed_early_reply_then_exact_prompt_read_and_zero_exit_is_rejected_on_replay() {
        let root = tempfile::tempdir().unwrap();
        let adapter = fixture("before-prompt", root.path(), None);
        let staging = tempfile::tempdir().unwrap();
        let mut child = command(&adapter.node, staging.path()).unwrap();
        let (advance, observed) = mpsc::channel();
        let prompt = json!({"schema":PROTOCOL,"type":"prompt","text":"PRIVATE exact \u{2028} prompt","prompt_nonce":digest(b"fresh delivered prompt")});
        let root_path = root.path();
        let staging_path = staging.path();
        let sent_prompt = prompt.clone();
        let deadline = Instant::now() + Duration::from_secs(30);
        let output = af_process::run_supervised_duplex(
            &mut child, Duration::from_secs(30), ExitPolicy::KillProcessGroup,
            move |writer| -> std::io::Result<()> {
                write_record(writer, &init(&adapter.config, root_path, &adapter.captured.auth_context_id, root_path, staging_path, None, false, false, &[]))?;
                let wait = || observed.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(std::io::Error::other);
                wait()?;
                write_record(writer, &json!({"schema":PROTOCOL,"type":"prepare","model":"o3","effort":"high","model_identity":digest(b"model")}))?;
                // Delivery is causally after the reader captured every early response byte.
                wait()?;
                write_record(writer, &sent_prompt)
            },
            move |reader| -> std::io::Result<Vec<Vec<u8>>> {
                let mut reader = BufReader::new(reader);
                let mut records = Vec::new();
                loop {
                    let mut line = Vec::new();
                    let count = reader.by_ref().take((MAX_RECORD + 1) as u64).read_until(b'\n', &mut line)?;
                    if count == 0 { break; }
                    if count > MAX_RECORD || !line.ends_with(b"\n") || records.len() >= 6 {
                        return Err(std::io::Error::other("unexpected fixture output"));
                    }
                    records.push(line);
                    if records.len() == 1 || records.len() == 6 {
                        advance.send(()).map_err(std::io::Error::other)?;
                    }
                }
                Ok(records)
            },
        ).unwrap();
        assert!(output.input.is_ok(), "{:?}", output.input);
        assert!(output.status.success(), "{:?}", output.status);
        assert!(!output.stderr_held && !output.truncation.is_truncated());
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        assert_eq!(
            std::fs::read(root.path().join("prompt")).unwrap(),
            prompt["text"].as_str().unwrap().as_bytes()
        );
        let records = output.output.unwrap();
        let values: Vec<Value> = records
            .iter()
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(
            values
                .iter()
                .map(|v| v["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "ready", "prepared", "accepted", "usage", "message", "settled"
            ]
        );
        assert_eq!(values[3]["complete"], true);
        let reply: Value = serde_json::from_str(values[4]["text"].as_str().unwrap()).unwrap();
        assert_eq!(reply["schema"], "af.worker-reply/1");
        assert_eq!(reply["outputs"]["report"][0]["summary"], "premature");
        let mut state = State::default();
        state.record(&records[0]).unwrap();
        state.record(&records[1]).unwrap();
        // Replay at the observed pre-delivery boundary through the production validator.
        assert_eq!(
            state.record(&records[2]).unwrap_err(),
            "Pi host lifecycle violation"
        );
        // Even a delayed parser, running after the write, rejects the stale challenge.
        state.prompt_sent = true;
        state.prompt_nonce = prompt["prompt_nonce"].as_str().map(str::to_owned);
        assert_eq!(
            state.record(&records[2]).unwrap_err(),
            "Pi host acknowledged a prompt it was not delivered"
        );
        assert!(!state.accepted && !state.settled);
        assert!(state.observation().reported_usage.is_none());
    }

    #[test]
    fn sdk_catalog_pages_preserve_record_bounds_and_select_models_beyond_the_first_page() {
        let root = tempfile::tempdir().unwrap();
        let adapter = fixture("pages-valid", root.path(), None);
        let catalog = probe(
            &adapter.node,
            &adapter.config,
            &adapter.auth_dir,
            Duration::from_secs(30),
            None,
        )
        .unwrap();
        assert_eq!(catalog.models.len(), 70);
        assert!(catalog.next_offset.is_none());
        assert_eq!(
            catalog.model("~vendor/model-69", "high").unwrap().id,
            "~vendor/model-69"
        );
        let cas = Cas::open(root.path().join("cas")).unwrap();
        let returned = invoke_test(
            &adapter,
            &cas,
            root.path(),
            b"PRIVATE".to_vec(),
            Duration::from_secs(30),
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert!(returned.message.is_ok(), "{:?}", returned.message);
        assert_eq!(
            std::fs::read(root.path().join("prompt")).unwrap(),
            b"PRIVATE"
        );
        for mode in [
            "pages-offset",
            "pages-limit",
            "pages-null",
            "pages-duplicate",
            "pages-catalog",
            "pages-auth",
        ] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture(mode, root.path(), None);
            assert!(
                probe(
                    &adapter.node,
                    &adapter.config,
                    &adapter.auth_dir,
                    Duration::from_secs(30),
                    None
                )
                .is_err(),
                "{mode}"
            );
            assert!(!root.path().join("prompt").exists());
        }
    }

    #[test]
    fn identity_auth_context_backend_model_effort_and_version_drift_refuse_before_private_input() {
        for mode in [
            "identity",
            "auth-context",
            "backend",
            "model",
            "effort",
            "version",
            "prior-policy",
            "kimi-prior-policy",
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut adapter = fixture(mode, root.path(), None);
            if mode.ends_with("prior-policy") {
                // The preceding policy captured only HOST, omitting Rust accounting semantics.
                adapter.captured.adapter_policy_id = digest(HOST.as_bytes());
                assert_ne!(adapter.captured.adapter_policy_id, policy_id());
            }
            if mode == "auth-context" {
                adapter.auth_dir = root.path().join("different-auth-context");
                std::fs::create_dir(&adapter.auth_dir).unwrap();
            }
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let returned = invoke_test(
                &adapter,
                &cas,
                root.path(),
                b"PRIVATE".to_vec(),
                Duration::from_secs(30),
                WorkerAccess::WriteSource,
                None,
                &[],
            );
            assert!(returned.message.is_err(), "{mode}");
            assert!(!root.path().join("prompt").exists(), "{mode}");
            if mode.ends_with("prior-policy") {
                assert!(!root.path().join("started").exists());
            }
            assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 0);
        }
    }
    #[test]
    fn errors_eof_limits_and_malformed_records_preserve_observed_usage() {
        for mode in ["error", "eof", "after", "partial", "oversize", "malformed"] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture(mode, root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let returned = invoke_test(
                &adapter,
                &cas,
                root.path(),
                b"request".to_vec(),
                Duration::from_secs(30),
                WorkerAccess::ReadOnly,
                None,
                &[],
            );
            assert!(returned.message.is_err(), "{mode}");
            let observed = returned.usage_observation.unwrap();
            assert_eq!(
                observed.reported_usage.unwrap().chargeable_tokens.get(),
                9,
                "{mode}"
            );
            assert!(!observed.charge_complete, "{mode}");
        }
    }
    // Readiness comes from the host, but neither accept nor a connected peer may
    // outlive the bounded wait. Completion wakes the wait on early host failure.
    fn cancellation_pid(
        listener: &std::os::unix::net::UnixListener,
        completed: &mpsc::Receiver<()>,
    ) -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut peer = None;
        let mut pid = Vec::new();
        loop {
            if peer.is_none() {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(true).map_err(|e| e.to_string())?;
                        peer = Some(stream);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            if let Some(stream) = &mut peer {
                let mut bytes = [0; 32];
                match stream.read(&mut bytes) {
                    Ok(0) => {
                        let pid = String::from_utf8(pid).map_err(|e| e.to_string())?;
                        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
                            return Err("host closed readiness without a PID".into());
                        }
                        return Ok(pid);
                    }
                    Ok(count) => {
                        pid.extend_from_slice(&bytes[..count]);
                        if pid.len() > 20 {
                            return Err("oversized readiness PID".into());
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("host readiness deadline elapsed".into());
            }
            match completed.recv_timeout(remaining.min(Duration::from_millis(10))) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("adapter completed before host readiness".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn cancel_fixture(mode: &str) -> Result<String, String> {
        use std::os::unix::net::UnixListener;
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("control");
        let listener = UnixListener::bind(&socket).unwrap();
        let adapter = fixture(mode, root.path(), Some(&socket));
        let cas = Cas::open(root.path().join("cas")).unwrap();
        let cancelled = AtomicBool::new(false);
        let (finished, completed) = mpsc::channel();
        std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                let returned = invoke_test(
                    &adapter,
                    &cas,
                    root.path(),
                    b"request".to_vec(),
                    Duration::from_secs(30),
                    WorkerAccess::ReadOnly,
                    Some(&cancelled),
                    &[],
                );
                let _ = finished.send(());
                returned
            });
            let pid = cancellation_pid(&listener, &completed);
            // Always cancel before joining, including every failed readiness path.
            cancelled.store(true, Ordering::Release);
            let returned = running.join().unwrap();
            assert!(returned.message.is_err());
            assert_eq!(returned.raw_artifact_ids.len(), 1);
            let evidence =
                PiInferenceEvidenceV2::decode(&cas.get(&returned.raw_artifact_ids[0]).unwrap())
                    .unwrap();
            assert!(!evidence.successful && !evidence.settled);
            assert_eq!(evidence.started_inferences, 1);
            assert_eq!(
                evidence
                    .aggregate
                    .reported_usage
                    .unwrap()
                    .chargeable_tokens
                    .get(),
                9
            );
            assert_eq!(
                returned
                    .usage_observation
                    .unwrap()
                    .reported_usage
                    .unwrap()
                    .chargeable_tokens
                    .get(),
                9
            );
            pid
        })
    }

    #[test]
    fn cancellation_during_inference_reaps_the_process_and_keeps_usage() {
        let pid = cancel_fixture("cancel").unwrap();
        let status = std::process::Command::new("/bin/kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "owned leader was reaped");
    }

    #[test]
    fn cancellation_readiness_observes_early_failure_before_accept_or_pid() {
        for mode in ["error", "cancel-no-pid"] {
            assert!(cancel_fixture(mode).is_err(), "{mode}");
        }
    }
}

#[test]
fn sdk_catalog_accepts_arbitrary_provider_models_but_only_honest_principals_and_declared_efforts() {
    for backend in [
        "kimi-coding",
        "anthropic",
        "openai",
        "amazon-bedrock",
        "team-compatible",
    ] {
        let mut catalog = ready();
        catalog.backend = backend.into();
        catalog.principal = None;
        catalog.auth_method = if backend == "kimi-coding" {
            "oauth"
        } else {
            "api_key"
        }
        .into();
        catalog.models[0].id = if backend == "kimi-coding" {
            "k3.1-pro"
        } else {
            "anthropic.claude-v1:0"
        }
        .into();
        catalog.models[0].efforts = vec!["low".into(), "high".into(), "max".into()];
        catalog.validate().unwrap();
        assert!(catalog.model(&catalog.models[0].id, "high").is_ok());
        assert!(catalog.model(&catalog.models[0].id, "medium").is_err());
        assert!(catalog.model("unknown", "high").is_err());
        let mut invented = catalog.clone();
        invented.principal = Some(digest(b"profile-name"));
        assert!(invented.validate().is_err());
        for invalid in ["ultra", "invalid"] {
            let mut bad = catalog.clone();
            bad.models[0].efforts.push(invalid.into());
            assert!(bad.validate().is_err());
        }
    }
    let mut openai = ready();
    openai.principal = None;
    assert!(openai.validate().is_err());
}

#[allow(clippy::too_many_arguments)]
fn invoke_test(
    adapter: &PiTaskAdapter,
    cas: &Cas,
    workdir: &Path,
    input: Vec<u8>,
    timeout: Duration,
    access: WorkerAccess,
    cancellation: Option<&AtomicBool>,
    environment: &[(String, String)],
) -> ModelWorkerReturn {
    adapter
        .invoke_with_allowance(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
            RemainingTokenAllowance::new(1_000_000).unwrap(),
            af_runner::task::reply_schema::ReplyContract::Request,
        )
        .unwrap_or_else(|unstarted| *unstarted.0)
}

#[test]
fn tool_count_evidence_accepts_only_declared_names_and_cumulative_counts() {
    let mut state = accepted();
    record(&mut state, "tool_count", json!({"name":"read","count":1})).unwrap();
    record(&mut state, "tool_count", json!({"name":"read","count":2})).unwrap();
    record(&mut state, "tool_count", json!({"name":"edit","count":1})).unwrap();
    for wrong in [
        json!({"name":"PRIVATE_NAME","count":1}),
        json!({"name":"read","count":2}),
        json!({"name":"read","count":3,"arguments":"PRIVATE_ARGS"}),
        json!({"name":"write","count":4097}),
    ] {
        assert!(record(&mut state, "tool_count", wrong).is_err());
    }
    assert_eq!(
        state.tool_counts,
        BTreeMap::from([("edit".into(), 1), ("read".into(), 2)])
    );
}
