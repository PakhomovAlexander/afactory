use super::*;
use std::os::unix::fs::PermissionsExt;

fn ready() -> Ready {
    Ready {
        principal: digest(b"account"),
        auth_method: "oauth".into(),
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

mod process {
    use super::*;
    fn fixture(mode: &str, root: &Path, socket: Option<&Path>) -> PiTaskAdapter {
        let node = root.join("node");
        let mut response = serde_json::to_value(ready()).unwrap();
        response["schema"] = json!(PROTOCOL);
        response["type"] = json!("ready");
        let body = r#"#!/usr/bin/python3 -B
import json,os,sys,socket,signal
mode=MODE
open(START_MARKER,'w').write('started')
ready=json.loads(READY)
def emit(v):
 v['schema']='af.pi-host/2'
 data=(json.dumps(v,ensure_ascii=False)+'\n').encode()
 # Split inside a UTF-8 sequence too: LF is the only record separator.
 for at in range(0,len(data),len(data) if mode=='immediate' else 7): os.write(1,data[at:at+7] if mode!='immediate' else data)
r=json.loads(sys.stdin.readline())
assert 'NODE_OPTIONS' not in os.environ and 'OPENAI_API_KEY' not in os.environ
if mode=='readonly': assert not r['writable']
if mode=='writable': assert r['writable']==[r['cwd']]
if mode=='identity': ready['principal']='sha256:'+'b'*64
if mode=='backend': ready['backend']='anthropic'
if mode=='version': ready['version']='1.5.0'
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
            backend: "openai-codex".into(),
            sdk_entrypoint: root.join("sdk/dist/index.js"),
        };
        let captured = authority(&node, &config, root, &ready().models[0]);
        PiTaskAdapter {
            node,
            config,
            auth_dir: root.into(),
            principal: digest(b"account"),
            model: "o3".into(),
            effort: "high".into(),
            captured,
        }
    }
    #[test]
    fn expired_or_cancelled_attempts_do_not_start_a_host() {
        for expired in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture("success", root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let cancelled = AtomicBool::new(!expired);
            let returned = adapter.invoke(
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
            let returned = adapter.invoke(
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
            let returned = adapter.invoke(
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
        let returned = adapter.invoke(
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
            let returned = adapter.invoke(
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
        let output = review_process::run_supervised_duplex(
            &mut child, Duration::from_secs(30), ExitPolicy::KillProcessGroup,
            move |writer| -> std::io::Result<()> {
                write_record(writer, &init(&adapter.config, root_path, root_path, staging_path, None, false, false, &[]))?;
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
    fn identity_backend_model_effort_and_version_drift_refuse_before_private_input() {
        for mode in ["identity", "backend", "model", "effort", "version"] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture(mode, root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let returned = adapter.invoke(
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
            assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 0);
        }
    }
    #[test]
    fn errors_eof_limits_and_malformed_records_preserve_observed_usage() {
        for mode in ["error", "eof", "after", "partial", "oversize", "malformed"] {
            let root = tempfile::tempdir().unwrap();
            let adapter = fixture(mode, root.path(), None);
            let cas = Cas::open(root.path().join("cas")).unwrap();
            let returned = adapter.invoke(
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
                let returned = adapter.invoke(
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
