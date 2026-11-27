//! Credential-free smoke using the installed app-server and the common Attempt reservation.
use super::subject::{Analyzer, Tools};
use super::*;
use af_runner_codex::navigation::{
    self as codex, Bindings, CallIntent, DurableAck, NativeHooks, NavigationHost, RawCallRecord,
    SemanticOperation, SemanticPosition, ToolSet,
};
use af_store::store::task::execution::PreparedTaskAttempt;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Mutex;
use std::time::Instant;

const INSTRUCTION: &str = "Return only a JSON object with anchors (an array of unique source-relative path and one-based line objects, sorted by path and line) and truncated (a boolean).";
pub(super) const MAX_SENDS: u32 = 8;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Configuration {
    executable: PathBuf,
    endpoint: SocketAddrV4,
    catalog_sha256: String,
    config: String,
}

impl Configuration {
    pub(super) fn capture(path: &Path, endpoint: Option<SocketAddrV4>) -> Result<Self, String> {
        let fixture = Self {
            executable: std::path::absolute(path).map_err(|e| e.to_string())?,
            endpoint: endpoint.ok_or("Missing dummy Responses endpoint")?,
            catalog_sha256: codex::sha256(codex::FIXTURE_MODEL_CATALOG),
            config: codex::config_identity(),
        };
        fixture.validate()?;
        read_bytes(
            &fixture.executable,
            codex::LINUX_X86_64_SHA256,
            512 * 1024 * 1024,
        )?;
        Ok(fixture)
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if !cfg!(all(target_os = "linux", target_arch = "x86_64"))
            || !self.executable.is_absolute()
            || *self.endpoint.ip() != Ipv4Addr::LOCALHOST
            || self.endpoint.port() == 0
            || self.catalog_sha256 != codex::sha256(codex::FIXTURE_MODEL_CATALOG)
            || self.config != codex::config_identity()
        {
            return Err(
                "Native fixture requires the pinned Linux transport and IPv4 loopback".into(),
            );
        }
        Ok(())
    }

    fn bindings(&self, source: &str, slot: &Slot, attempt_binding: String) -> Bindings {
        let question = question(slot);
        Bindings {
            task_attempt_invocation: attempt_binding,
            source: source.into(),
            prompt: codex::prompt_identity(&question),
            tools: tool_set(slot.arm).identity(),
            model_catalog: self.catalog_sha256.clone(),
            executable: codex::LINUX_X86_64_SHA256.into(),
            account: codex::FIXTURE_ACCOUNT.into(),
            protocol: codex::PROTOCOL.into(),
            config: self.config.clone(),
        }
    }
}

fn tool_set(arm: Arm) -> ToolSet {
    match arm {
        Arm::Baseline => ToolSet::Text,
        Arm::Treatment => ToolSet::Semantic,
    }
}

fn question(slot: &Slot) -> String {
    format!("{INSTRUCTION}\n\n{}", slot.case.question)
}

pub(super) struct Host<'a, 'tools> {
    pub(super) tools: Mutex<&'a mut Tools<'tools>>,
    pub(super) analyzer: Option<Analyzer<'a>>,
}

impl NavigationHost for Host<'_, '_> {
    fn navigate(&self, request: codex::Navigation, _: Instant) -> Result<String, String> {
        let name = match &request {
            codex::Navigation::ListPaths { .. } => "af_list_paths",
            codex::Navigation::ReadRange { .. } => "af_read_range",
            codex::Navigation::Search { .. } => "af_search",
        };
        let reply = self.tools.lock().unwrap().text(name, &request)?;
        serde_json::to_string(&reply).map_err(|e| e.to_string())
    }

    fn semantic(
        &self,
        operation: SemanticOperation,
        position: SemanticPosition,
        _: Instant,
    ) -> Result<String, String> {
        let analyzer = self.analyzer.as_ref().ok_or("Missing treatment analyzer")?;
        let operation = match operation {
            SemanticOperation::Definition => semantic::Operation::Definition,
            SemanticOperation::References => semantic::Operation::References,
            SemanticOperation::Implementation => semantic::Operation::Implementation,
        };
        let position = semantic::Position {
            path: position.path,
            line: position.line,
            character: position.character,
        };
        let reply = self
            .tools
            .lock()
            .unwrap()
            .semantic(operation, &position, analyzer)?;
        serde_json::to_string(&reply).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Send {
    intent: String,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    raw: Option<String>,
}

struct Hooks<'a> {
    cas: &'a Cas,
    expected: &'a Bindings,
    cancellation: Option<&'a std::sync::atomic::AtomicBool>,
    sends: Mutex<Vec<Send>>,
}

impl Hooks<'_> {
    fn retain(&self, value: &impl Serialize) -> Result<String, String> {
        let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
        let id = self.cas.put_json(&value).map_err(|e| e.to_string())?;
        self.cas.flush().map_err(|e| e.to_string())?;
        Ok(id)
    }
}

impl NativeHooks for Hooks<'_> {
    fn before_send(&self, intent: &CallIntent, deadline: Instant) -> Result<DurableAck, String> {
        let mut sends = self.sends.lock().unwrap();
        if Instant::now() >= deadline
            || self
                .cancellation
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            || intent.bindings != *self.expected
            || sends.len() >= MAX_SENDS as usize
            || intent.ordinal as usize != sends.len() + 1
            || sends.last().is_some_and(|send| send.raw.is_none())
        {
            return Err("Call lacks its exact available fixture scope".into());
        }
        sends.push(Send {
            intent: self.retain(intent)?,
            raw: None,
        });
        Ok(DurableAck {
            intent_id: intent.id.clone(),
        })
    }

    fn collect(&self, record: &RawCallRecord) -> Result<DurableAck, String> {
        let mut sends = self.sends.lock().unwrap();
        let send = sends.last_mut().ok_or("No fixture intent")?;
        let intent: CallIntent = read(self.cas, &send.intent)?;
        if intent != record.intent || send.raw.is_some() {
            return Err("Raw call differs from the durable intent".into());
        }
        send.raw = Some(self.retain(record)?);
        Ok(DurableAck {
            intent_id: intent.id,
        })
    }
}

fn read<T: serde::de::DeserializeOwned>(cas: &Cas, id: &str) -> Result<T, String> {
    serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

pub(super) fn evidence(sends: &[Send]) -> impl Iterator<Item = &String> {
    sends
        .iter()
        .flat_map(|send| std::iter::once(&send.intent).chain(send.raw.as_ref()))
}

pub(super) fn inclusive(
    cas: &Cas,
    sends: &[Send],
    expected: &Bindings,
) -> Result<Option<u64>, String> {
    let mut ids = BTreeSet::new();
    let mut total = 0_u64;
    let mut complete = true;
    let mut inferred = false;
    if sends.len() > MAX_SENDS as usize {
        return Err("Too many fixture sends".into());
    }
    for (index, send) in sends.iter().enumerate() {
        let intent: CallIntent = read(cas, &send.intent)?;
        if intent.bindings != *expected
            || intent.ordinal as usize != index + 1
            || !af_contracts::is_digest(&intent.request_sha256)
            || !ids.insert(intent.id.clone())
            || !matches!(intent.route.as_str(), "/models" | "/responses")
        {
            return Err("Invalid retained fixture intent".into());
        }
        let Some(raw) = &send.raw else {
            if index + 1 != sends.len() {
                return Err("Fixture sent an inference after missing usage".into());
            }
            complete = false;
            continue;
        };
        let raw: RawCallRecord = read(cas, raw)?;
        if raw.intent != intent
            || raw.drained_mono_ns < intent.started_mono_ns
            || !af_contracts::is_digest(&raw.response_sha256)
            || (intent.route == "/models" && raw.usage.is_some())
        {
            return Err("Raw call does not bind the retained intent".into());
        }
        if intent.route == "/responses" {
            inferred = true;
            match raw
                .usage
                .filter(|usage| {
                    usage
                        .failure
                        .as_deref()
                        .is_none_or(|failure| failure == "upstream terminal failure")
                })
                .and_then(|usage| usage.reconciled)
            {
                Some(usage) => {
                    let inclusive = usage
                        .input
                        .checked_add(usage.output)
                        .filter(|n| *n == usage.inclusive)
                        .ok_or("Contradictory token counters")?;
                    if usage
                        .input
                        .checked_sub(usage.cached_input)
                        .and_then(|input| input.checked_add(usage.output))
                        != Some(usage.chargeable)
                        || usage
                            .reasoning_output
                            .is_some_and(|reasoning| reasoning > usage.output)
                    {
                        return Err("Contradictory chargeable counters".into());
                    }
                    total = total.checked_add(inclusive).ok_or("Token total overflow")?;
                }
                None => complete = false,
            }
        }
    }
    Ok((complete && inferred).then_some(total))
}

pub(super) fn attempt_binding(task: &str, attempt: &str, context: &str, slot: &Slot) -> String {
    format!("{task}:{attempt}:{context}:{}", slot.index)
}

impl Configuration {
    pub(super) fn inclusive(
        &self,
        cas: &Cas,
        source: &str,
        slot: &Slot,
        sends: &[Send],
        attempt_binding: String,
    ) -> Result<Option<u64>, String> {
        inclusive(cas, sends, &self.bindings(source, slot, attempt_binding))
    }

    pub(super) fn collect(
        &self,
        cas: &Cas,
        slot: &Slot,
        tools: &mut Tools<'_>,
        attempt: &PreparedTaskAttempt,
        analyzer: Option<Analyzer<'_>>,
    ) -> (Vec<Send>, Result<semantic::Reply, String>) {
        let deadline = tools.deadline;
        let bindings = self.bindings(
            tools.broker.source_identity(),
            slot,
            attempt_binding(attempt.task_id(), attempt.id(), attempt.context_id(), slot),
        );
        let cancellation = tools.cancellation;
        let host = Host {
            tools: Mutex::new(tools),
            analyzer,
        };
        let hooks = Hooks {
            cas,
            expected: &bindings,
            cancellation,
            sends: Mutex::new(Vec::new()),
        };
        let request = codex::Request {
            profile: codex::WorkerProfile::Subject,
            pin: codex::Pin {
                executable: self.executable.clone(),
                executable_sha256: codex::LINUX_X86_64_SHA256.into(),
                catalog: codex::FIXTURE_MODEL_CATALOG.to_vec(),
                catalog_sha256: self.catalog_sha256.clone(),
            },
            native_auth_home: PathBuf::new(),
            bindings: bindings.clone(),
            question: question(slot),
            tools: tool_set(slot.arm),
            deadline,
            max_inclusive_tokens: attempt.reservation().tokens,
            max_sends: MAX_SENDS,
        };
        let result = (|| {
            if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
                || request.max_inclusive_tokens != FIXTURE_TOKENS
            {
                return Err("Fixture Attempt lacks its exact allowance".into());
            }
            let outcome = codex::run_fixture(request, self.endpoint, &host, &hooks)
                .map_err(|e| e.to_string())?;
            if let Some(failure) = outcome.failure {
                return Err(failure);
            }
            if !outcome.cleanup_confirmed
                || inclusive(cas, &hooks.sends.lock().unwrap(), &bindings)?.is_none()
            {
                return Err("Fixture invocation lost cleanup or usage evidence".into());
            }
            let answer = outcome.answer.ok_or("Missing native answer")?;
            serde_json::from_str(&answer).map_err(|_| "Unstructured native anchor answer".into())
        })();
        (hooks.sends.into_inner().unwrap(), result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex::{CallRecordSchema, RawField, RawUsage, ReconciledUsage};
    use std::time::Duration;

    #[test]
    fn durable_usage_is_bound_to_each_intent_and_missing_usage_stays_unknown() {
        let state = tempfile::tempdir().unwrap();
        let cas = Cas::open(state.path()).unwrap();
        let (_, cases, _) = subject::smoke(&cas).unwrap();
        let slots = schedule(&cases);
        let fixture = Configuration {
            executable: "/fixture-codex".into(),
            endpoint: "127.0.0.1:1".parse().unwrap(),
            catalog_sha256: codex::sha256(codex::FIXTURE_MODEL_CATALOG),
            config: codex::config_identity(),
        };
        let expected = fixture.bindings(&cases.source_digest, &slots[0], "attempt-fixture".into());
        let hooks = Hooks {
            cas: &cas,
            expected: &expected,
            cancellation: None,
            sends: Mutex::new(Vec::new()),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut intent = CallIntent {
            id: "call-1".into(),
            ordinal: 1,
            route: "/responses".into(),
            request_sha256: codex::sha256(b"synthetic HTTP body"),
            bindings: expected.clone(),
            started_mono_ns: 100,
        };
        intent.bindings.prompt = codex::sha256(b"unbound");
        assert!(hooks.before_send(&intent, deadline).is_err());
        intent.bindings = expected.clone();
        assert_eq!(
            hooks.before_send(&intent, deadline).unwrap().intent_id,
            intent.id
        );
        assert!(hooks.before_send(&intent, deadline).is_err());

        let mut record = RawCallRecord {
            schema: CallRecordSchema::V1,
            intent: intent.clone(),
            http_status: Some(200),
            response_sha256: codex::sha256(b"synthetic HTTP response"),
            response_bytes: 23,
            usage: Some(RawUsage {
                observations: vec![],
                terminal_event: Some("response.completed".into()),
                value: RawField::Present(
                    json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":20}}),
                ),
                reconciled: Some(ReconciledUsage {
                    input: 100,
                    cached_input: 20,
                    output: 10,
                    reasoning_output: None,
                    inclusive: 110,
                    chargeable: 90,
                }),
                failure: None,
            }),
            drained_mono_ns: 200,
            failure: None,
        };
        hooks.collect(&record).unwrap();
        assert!(hooks.collect(&record).is_err());
        {
            let sends = hooks.sends.lock().unwrap();
            assert_eq!(inclusive(&cas, &sends, &expected).unwrap(), Some(110));
            let ids = evidence(&sends).collect::<Vec<_>>();
            assert_eq!(read::<CallIntent>(&cas, ids[0]).unwrap(), intent);
            assert_eq!(read::<RawCallRecord>(&cas, ids[1]).unwrap(), record);
        }

        intent.ordinal = 2;
        intent.id = "call-2".into();
        hooks.before_send(&intent, deadline).unwrap();
        record.intent = intent;
        record.usage = None;
        assert_eq!(
            inclusive(&cas, &hooks.sends.lock().unwrap(), &expected).unwrap(),
            None
        );
        hooks.collect(&record).unwrap();
        let sends = hooks.sends.into_inner().unwrap();
        assert_eq!(inclusive(&cas, &sends, &expected).unwrap(), None);
        assert!(
            inclusive(
                &cas,
                &sends,
                &fixture.bindings(&cases.source_digest, &slots[1], "attempt-fixture".into())
            )
            .is_err()
        );
    }
}
