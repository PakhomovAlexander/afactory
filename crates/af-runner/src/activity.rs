//! Metadata-only live activity. Untrusted stdout stays in a bounded, disposable observer;
//! only known notification kinds become closed enums. No argument, output, prompt, provider
//! identifier or reasoning string is copied into the sidecar, even before credential admission.
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use af_process::{ObservedStream, OutputObserver};
use af_store::activity::{Binding, Operation, Phase, Writer};
use serde::Deserialize;

pub const QUEUE_BYTES: u64 = 256 * 1024;
pub const FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct Sink(Arc<Mutex<Writer>>);
impl Sink {
    /// A failed optional channel must never change execution or credential admission.
    pub fn create(binding: Binding) -> Option<Self> {
        Writer::create(binding)
            .ok()
            .map(|w| Self(Arc::new(Mutex::new(w))))
    }
    fn publish(&self, operation: Operation, phase: Phase, arrival: Option<Instant>) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        // Timestamp the drain's observation, not a delayed consumer's disk write.
        let age = arrival.map_or(0, |at| {
            u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        if let Ok(mut writer) = self.0.lock() {
            let _ = writer.publish(operation, phase, now.saturating_sub(age));
        }
    }
}

/// Framing is independent of read chunk boundaries. Oversize frames and dropped chunks discard
/// through the next newline, so a suffix cannot be mistaken for a complete notification.
#[derive(Default)]
struct Frames {
    pending: Vec<u8>,
    discarding: bool,
    offset: u64,
}
impl Frames {
    fn chunk(&mut self, offset: u64, bytes: &[u8], mut emit: impl FnMut(Operation, Phase)) {
        if offset != self.offset {
            self.pending.clear();
            self.discarding = true;
        }
        self.offset = offset.saturating_add(bytes.len() as u64);
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            let ends = part.last() == Some(&b'\n');
            if !self.discarding {
                if self.pending.len() + part.len() > FRAME_BYTES {
                    self.pending.clear();
                    self.discarding = true;
                    emit(Operation::Observation, Phase::Truncated);
                } else {
                    self.pending.extend_from_slice(part);
                    if ends {
                        if let Some((operation, phase)) = codex(&self.pending) {
                            emit(operation, phase);
                        }
                        self.pending.clear();
                    }
                }
            }
            if ends {
                self.discarding = false;
            }
        }
    }
}

fn codex(bytes: &[u8]) -> Option<(Operation, Phase)> {
    #[derive(Deserialize)]
    struct Item<'a> {
        #[serde(rename = "type", borrow)]
        kind: Option<&'a str>,
        #[serde(borrow)]
        status: Option<&'a str>,
        exit_code: Option<i64>,
    }
    #[derive(Deserialize)]
    struct Message<'a> {
        #[serde(rename = "type", borrow)]
        kind: Option<&'a str>,
        #[serde(borrow)]
        item: Option<Item<'a>>,
    }
    let message: Message<'_> = serde_json::from_slice(bytes).ok()?;
    let phase = match message.kind? {
        "turn.started" => return Some((Operation::Worker, Phase::Started)),
        "turn.completed" => return Some((Operation::Worker, Phase::Completed)),
        "turn.failed" => return Some((Operation::Worker, Phase::Failed)),
        "item.started" => Phase::Started,
        "item.completed" => Phase::Completed,
        _ => return None,
    };
    let item = message.item?;
    let operation = match item.kind? {
        "command_execution" => Operation::Command,
        "file_change" => Operation::FileChange,
        "mcp_tool_call" => Operation::Tool,
        _ => return None,
    };
    let phase = if phase == Phase::Completed
        && (item.status == Some("failed") || item.exit_code.is_some_and(|code| code != 0))
    {
        Phase::Failed
    } else {
        phase
    };
    Some((operation, phase))
}

pub(crate) struct Observation {
    observer: OutputObserver,
}
impl Observation {
    pub(crate) fn start(sink: Sink) -> Option<Self> {
        let (send, receive) = mpsc::sync_channel::<af_process::ObservedOutput>(1);
        let thread = std::thread::Builder::new()
            .name("af-activity".into())
            .spawn(move || {
                let Ok(output) = receive.recv() else {
                    return;
                };
                let mut frames = Frames::default();
                let mut dropped = 0;
                loop {
                    let closed = output.closed();
                    let lost = output.dropped_chunks();
                    if lost != dropped {
                        sink.publish(Operation::Observation, Phase::Dropped, None);
                        dropped = lost;
                    }
                    while let Some(chunk) = output.try_take() {
                        if chunk.stream == ObservedStream::Stdout {
                            frames.chunk(chunk.byte_offset, &chunk.bytes, |op, phase| {
                                sink.publish(op, phase, Some(chunk.arrival))
                            });
                        }
                    }
                    // Failed offers do not wake a consumer. Recheck after draining so an
                    // overflow followed by a quiet, still-running Worker is visible now.
                    let lost = output.dropped_chunks();
                    if lost != dropped {
                        sink.publish(Operation::Observation, Phase::Dropped, None);
                        dropped = lost;
                    }
                    if closed {
                        if !frames.pending.is_empty() {
                            sink.publish(Operation::Observation, Phase::Truncated, None);
                        }
                        break;
                    }
                    std::thread::park();
                }
            })
            .ok()?;
        let (observer, output) = af_process::observer(QUEUE_BYTES, Some(thread.thread().clone()));
        send.send(output).ok()?;
        Some(Self { observer })
    }
    pub(crate) fn observer(&self) -> &OutputObserver {
        &self.observer
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.observer.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use af_store::activity::{Reader, SCHEMA};
    use serde_json::json;

    #[test]
    fn codex_projection_has_no_free_form_provider_text() {
        let mut events = Vec::new();
        let mut frames = Frames::default();
        let bytes = [
            json!({"type":"item.started","item":{"type":"command_execution","command":"SECRET\u{1b}[2J","arguments":"private","reasoning":"private"}}),
            json!({"type":"item.completed","item":{"type":"command_execution","exit_code":9,"aggregated_output":"SECRET"}}),
            json!({"type":"item.completed","item":{"type":"file_change","status":"completed","changes":[{"path":"SECRET"}]}}),
            json!({"type":"item.started","item":{"type":"mcp_tool_call","tool":"SECRET","arguments":{"token":"SECRET"}}}),
            json!({"type":"item.started","item":{"type":"SECRET"}}),
            json!({"type":"item.completed","item":{"type":"reasoning","text":"SECRET"}}),
            json!({"type":"SECRET"}),
        ].iter().map(|v| format!("{v}\n")).collect::<String>();
        for (n, chunk) in bytes.as_bytes().chunks(7).enumerate() {
            frames.chunk((n * 7) as u64, chunk, |op, phase| events.push((op, phase)));
        }
        assert_eq!(
            events,
            [
                (Operation::Command, Phase::Started),
                (Operation::Command, Phase::Failed),
                (Operation::FileChange, Phase::Completed),
                (Operation::Tool, Phase::Started)
            ]
        );
        assert!(frames.pending.is_empty());
    }

    #[test]
    fn oversized_and_gapped_frames_have_bounded_memory_and_never_parse_a_suffix() {
        let mut frames = Frames::default();
        let mut events = Vec::new();
        frames.chunk(0, &vec![b'x'; FRAME_BYTES * 4], |o, p| events.push((o, p)));
        assert!(frames.pending.capacity() <= FRAME_BYTES);
        assert_eq!(events, [(Operation::Observation, Phase::Truncated)]);
        let good = b"{\"type\":\"item.started\",\"item\":{\"type\":\"command_execution\"}}\n";
        // Even a valid JSON suffix is discarded after an oversized frame or an offset gap.
        frames.chunk((FRAME_BYTES * 4) as u64, good, |o, p| events.push((o, p)));
        frames.chunk(999_999, good, |o, p| events.push((o, p)));
        assert_eq!(events.len(), 1);
        frames.chunk(999_999 + good.len() as u64, good, |o, p| {
            events.push((o, p))
        });
        assert_eq!(events.last(), Some(&(Operation::Command, Phase::Started)));
    }

    #[test]
    fn activity_writer_reader_and_schema_agree_and_reject_old_versions() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../schemas/task-activity-v1.json")).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let root = tempfile::tempdir().unwrap();
        let binding = Binding::new(root.path(), "task", "attempt").unwrap();
        let mut writer = Writer::create(binding.clone()).unwrap();
        for (op, phase) in [
            (Operation::Worker, Phase::Started),
            (Operation::Command, Phase::Failed),
            (Operation::FileChange, Phase::Completed),
            (Operation::Tool, Phase::Started),
            (Operation::Observation, Phase::Dropped),
            (Operation::Observation, Phase::Truncated),
        ] {
            writer.publish(op, phase, 1).unwrap();
        }
        let mut reader = Reader::new(binding);
        reader.poll().unwrap();
        for record in &reader.records {
            let value = serde_json::to_value(record).unwrap();
            assert!(validator.is_valid(&value));
            assert_eq!(value["schema"], SCHEMA);
            let mut old = value.clone();
            old["schema"] = json!("af/task-activity/0");
            assert!(!validator.is_valid(&old));
            let mut extra = value.clone();
            extra["arguments"] = json!("secret");
            assert!(!validator.is_valid(&extra));
            assert!(serde_json::from_value::<af_store::activity::Record>(extra).is_err());
            let mut unknown = value;
            unknown["operation"] = json!("secret");
            assert!(!validator.is_valid(&unknown));
            assert!(serde_json::from_value::<af_store::activity::Record>(unknown).is_err());
        }
    }
}
