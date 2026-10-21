//! The harness stream tap (design §7.3, package P6): incremental framing with whole-frame
//! withholding, the gap protocol, live records of validated structure only with correlation ids
//! in place of the harness's identifiers, pending content under the Attempt's allowance, and
//! Telemetry's own release decision.

use review_telemetry::blob::Media;
use review_telemetry::level::Level;
use review_telemetry::record::{Attrs, Record, SpanKind};
use review_telemetry::segment::writer::Writer;
use review_telemetry::stream::{Harness, Release, Released, Stream, Tap, TapOptions, corr};
use review_telemetry::subscriber::{RecordedSpan, SpanIdentity};
use serde_json::{Value, json};

/// A writer with an open `af.harness` span, and the tap's options below it.
fn harness(root: &std::path::Path, level: Level, kind: Harness) -> (Writer, TapOptions) {
    let mut options = crate::options(root, level);
    options.content_max_blob_bytes = 64 * 1024;
    let writer = Writer::new(options).unwrap();
    let span_id = writer
        .span_start(
            Some("0123456789abcdef0123456789abcdef"),
            None,
            "af.harness",
            SpanKind::Process,
            Attrs::new().id("af.run_id", "task:sha256:".to_string() + &"a".repeat(64)),
        )
        .unwrap();
    let options = TapOptions {
        harness: kind,
        parent: SpanIdentity {
            span: RecordedSpan {
                span_id,
                trace_id: Some("0123456789abcdef0123456789abcdef".into()),
            },
            identity: Attrs::new()
                .id("af.run_id", "task:sha256:".to_string() + &"a".repeat(64))
                .0,
            depth: 4,
        },
        provider: match kind {
            Harness::Claude => "anthropic",
            Harness::Codex => "openai",
        },
        bound_model: Some("claude-fixture-1".into()),
        tools: match kind {
            Harness::Claude => vec!["Read".into(), "Bash".into()],
            Harness::Codex => vec!["command_execution".into(), "mcp_tool_call".into()],
        },
        frame_max_bytes: 64 * 1024,
        content_allowance_bytes: 16 << 20,
        grants: vec![("CODEX_HOME".into(), "/grants/codex-home-value".into())],
        content_reasoning: true,
        content_tool_io: true,
        scope: review_telemetry::content::Scope::Process,
    };
    (writer, options)
}

/// Feed `lines` to `tap` as one chunk each, arriving 1 ms apart from `start`.
fn feed(tap: &mut Tap, writer: &Writer, stream: Stream, lines: &[String], start: u128) -> u64 {
    let mut offset = 0;
    for (index, line) in lines.iter().enumerate() {
        let bytes = format!("{line}\n");
        tap.chunk(
            writer,
            stream,
            offset,
            bytes.as_bytes(),
            start + index as u128 * 1_000_000,
        );
        offset += bytes.len() as u64;
    }
    offset
}

/// Every record the writer has written so far, from its live file.
fn live(writer: &Writer) -> (Vec<Record>, String) {
    writer.flush();
    let path = writer.path().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let records = review_telemetry::segment::reader::read_live(&path)
        .unwrap()
        .records;
    (records, text)
}

fn spans<'a>(records: &'a [Record], name: &str) -> Vec<&'a review_telemetry::record::SpanStart> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::SpanStart(start) if start.name == name => Some(start),
            _ => None,
        })
        .collect()
}

fn end_of<'a>(records: &'a [Record], span_id: &str) -> &'a review_telemetry::record::SpanEnd {
    records
        .iter()
        .find_map(|record| match record {
            Record::SpanEnd(end) if end.span_id == span_id => Some(end),
            _ => None,
        })
        .unwrap_or_else(|| panic!("span {span_id} ended"))
}

fn events<'a>(records: &'a [Record], name: &str) -> Vec<&'a review_telemetry::record::Event> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Event(event) if event.name == name => Some(event),
            _ => None,
        })
        .collect()
}

fn release(stdout: u64, stderr: u64, withheld: bool) -> Release {
    Release {
        withheld,
        native_withheld: withheld,
        stdout_bytes: stdout,
        stderr_bytes: stderr,
    }
}

/// A Claude `stream-json --verbose` session: init, a thinking and tool-calling turn, the tool
/// results, a final answer, the result.
fn claude_session(secret_text: &str) -> Vec<String> {
    [
        json!({"type":"system","subtype":"init","session_id":"0f5b3c1e-6a7d-4e8f-9a0b-1c2d3e4f5a6b",
               "model":"claude-fixture-1","tools":["Read","Bash","Task"],"mcp_servers":[],
               "claude_code_version":"2.1.303"}),
        json!({"type":"assistant","message":{"id":"msg_01FixtureTurnOne","model":"claude-fixture-1",
               "stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":20,
               "cache_read_input_tokens":300,"cache_creation_input_tokens":4},
               "content":[{"type":"thinking","thinking":format!("thinking {secret_text}")},
                          {"type":"text","text":"reading two files"},
                          {"type":"tool_use","id":"toolu_01FixtureRead","name":"Read",
                           "input":{"file_path":"src/lib.rs"}},
                          {"type":"tool_use","id":"toolu_01FixtureOther","name":"Unlisted",
                           "input":{"command":secret_text}}]}}),
        json!({"type":"user","message":{"content":[
               {"type":"tool_result","tool_use_id":"toolu_01FixtureRead","content":"fn main() {}"},
               {"type":"tool_result","tool_use_id":"toolu_01FixtureOther","content":"denied",
                "is_error":true}]}}),
        json!({"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,
               "retry_delay_ms":500,"error_status":529,"error":"overloaded"}),
        json!({"type":"assistant","message":{"id":"msg_01FixtureTurnTwo","model":"claude-fixture-1",
               "stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":7,
               "cache_read_input_tokens":320,"cache_creation_input_tokens":0},
               "content":[{"type":"text","text":format!("the answer {secret_text}")}]}}),
        json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour","resetsAt":1700000000}}),
        json!({"type":"result","subtype":"success","is_error":false,"num_turns":2,"duration_ms":1234,
               "result":format!("the answer {secret_text}"),"session_id":"0f5b3c1e-6a7d-4e8f-9a0b-1c2d3e4f5a6b"}),
    ]
    .iter()
    .map(Value::to_string)
    .collect()
}

#[test]
fn telemetry_claude_messages_become_spans() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, options) = harness(dir.path(), Level::Spans, Harness::Claude);
    let mut tap = Tap::new(&writer, options);
    let secret = "model-authored words never live";
    let lines = claude_session(secret);
    let start = 1_700_000_000_000_000_000;
    let bytes = feed(&mut tap, &writer, Stream::Stdout, &lines, start);
    let (_, before) = live(&writer);
    for original in [
        "model-authored",
        "msg_01Fixture",
        "toolu_01Fixture",
        "0f5b3c1e",
    ] {
        assert!(!before.contains(original), "{original} is never live");
    }
    let released = tap.finish(&writer, &release(bytes, 0, false));
    let (records, text) = live(&writer);
    assert!(
        !text.contains("model-authored"),
        "untrusted text is never published"
    );
    let turns = spans(&records, "af.turn");
    assert_eq!(turns.len(), 2, "one turn per assistant message");
    let segment = writer.segment_id().unwrap();
    // The first turn: from the init's arrival to its message's, with the harness's usage.
    assert_eq!(turns[0].start_unix_ns, start.to_string());
    let first = end_of(&records, &turns[0].span_id);
    assert_eq!(
        first.end_unix_ns.as_deref(),
        Some((start + 1_000_000).to_string().as_str())
    );
    let attributes = &first.attributes;
    assert_eq!(attributes["af.usage.input_tokens"], "10");
    assert_eq!(attributes["af.usage.output_tokens"], "20");
    assert_eq!(attributes["af.usage.cache_read_tokens"], "300");
    assert_eq!(attributes["af.usage.cache_write_tokens"], "4");
    assert_eq!(attributes["gen_ai.usage.input_tokens"], "314");
    assert_eq!(
        attributes["gen_ai.response.finish_reasons"],
        json!(["tool_use"])
    );
    assert_eq!(attributes["gen_ai.response.model"], "claude-fixture-1");
    assert_eq!(attributes["af.turn.thinking_blocks"], 1);
    assert_eq!(attributes["af.turn.thinking_bytes"], 9 + secret.len());
    assert_eq!(attributes["af.turn.tool_calls"], 2);
    assert_eq!(
        attributes["af.corr"],
        corr(&segment, "msg_01FixtureTurnOne")
    );
    assert_eq!(turns[0].attributes["gen_ai.operation.name"], "chat");
    assert_eq!(turns[0].attributes["af.series"], "turn");
    // The second turn begins when the tool results go back to the model.
    assert_eq!(turns[1].start_unix_ns, (start + 2_000_000).to_string());
    let second = end_of(&records, &turns[1].span_id);
    assert_eq!(
        second.attributes["gen_ai.response.finish_reasons"],
        json!(["end_turn"])
    );
    // One tool per `tool_use`, closed by its `tool_result` at that line's arrival.
    let tools = spans(&records, "af.tool");
    assert_eq!(tools.len(), 2);
    assert_eq!(
        tools[0].parent_span_id.as_deref(),
        Some(turns[0].span_id.as_str())
    );
    assert_eq!(tools[0].attributes["gen_ai.tool.name"], "Read");
    assert_eq!(
        tools[0].attributes["gen_ai.tool.call.id"],
        corr(&segment, "toolu_01FixtureRead")
    );
    assert_eq!(tools[0].attributes["af.tool.input_bytes"], 26);
    assert_eq!(tools[1].attributes["af.tool.unlisted"], true);
    assert!(tools[1].attributes.get("gen_ai.tool.name").is_none());
    let read = end_of(&records, &tools[0].span_id);
    assert_eq!(
        read.end_unix_ns.as_deref(),
        Some((start + 2_000_000).to_string().as_str())
    );
    assert_eq!(read.attributes["af.tool.output_bytes"], 12);
    assert_eq!(read.attributes["af.outcome"], "succeeded");
    assert_eq!(
        end_of(&records, &tools[1].span_id).attributes["af.outcome"],
        "failed"
    );
    // The other lines are events with their structure.
    assert_eq!(events(&records, "af.harness.init").len(), 1);
    assert_eq!(
        events(&records, "af.turn.api_retry")[0].span_id,
        turns[1].span_id
    );
    assert_eq!(
        events(&records, "af.harness.rate_limit")[0].attributes["af.rate_limit.status"],
        "allowed"
    );
    let result = &events(&records, "af.harness.result")[0].attributes;
    assert_eq!(result["af.result.subtype"], "success");
    assert_eq!(result["af.result.num_turns"], 2);
    let ids = &events(&records, "af.harness.ids")[0].attributes;
    assert_eq!(ids["af.stream.withheld"], false);
    assert_eq!(ids["af.stream.turns"], 2);
    assert_eq!(ids["af.stream.analysis"], "complete");
    // At `spans` no content is pended; only the identifiers are, for the ids event.
    assert!(released.contents.is_empty());
    assert!(
        released
            .ids
            .iter()
            .any(|(_, original)| original == "toolu_01FixtureRead")
    );
}

/// Design §7.3: an `assistant` message ends its turn as soon as the line is processed — the
/// live records show the turn ended at that line's arrival before any later `user` or `result`
/// line — its tool calls hang below that ended turn, the next turn opens when the tool results
/// go back, and a further line of the same message (Claude prints one per content block)
/// continues the ended turn as one `af.turn.continued` event instead of opening another.
#[test]
fn telemetry_claude_turn_ends_with_its_assistant_message() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, options) = harness(dir.path(), Level::Spans, Harness::Claude);
    let mut tap = Tap::new(&writer, options);
    let segment = writer.segment_id().unwrap();
    let start: u128 = 1_700_000_000_000_000_000;
    let ms = |n: u128| start + n * 1_000_000;
    let mut offset = 0;
    let mut send = |tap: &mut Tap, line: Value, at: u128| {
        let bytes = format!("{line}\n");
        tap.chunk(&writer, Stream::Stdout, offset, bytes.as_bytes(), at);
        offset += bytes.len() as u64;
    };
    send(
        &mut tap,
        json!({"type":"system","subtype":"init","session_id":"session-live"}),
        ms(0),
    );
    send(
        &mut tap,
        json!({"type":"assistant","message":{"id":"msg_live_one","model":"claude-fixture-1",
               "usage":{"input_tokens":3,"output_tokens":1},
               "content":[{"type":"thinking","thinking":"plan"},{"type":"text","text":"looking"}]}}),
        ms(1),
    );
    // After the assistant line, before any later line: the turn has ended, at its arrival.
    let (records, _) = live(&writer);
    let turns = spans(&records, "af.turn");
    assert_eq!(turns.len(), 1);
    let first = end_of(&records, &turns[0].span_id);
    assert_eq!(
        first.end_unix_ns.as_deref(),
        Some(ms(1).to_string().as_str())
    );
    assert_eq!(first.attributes["af.outcome"], "succeeded");
    assert_eq!(first.attributes["af.turn.text_blocks"], 1);
    assert_eq!(first.attributes["af.corr"], corr(&segment, "msg_live_one"));
    // A further line of the same message: its tool call below the ended turn, its counts on
    // the turn's `af.turn.continued` event, and no other turn.
    send(
        &mut tap,
        json!({"type":"assistant","message":{"id":"msg_live_one","model":"claude-fixture-1",
               "stop_reason":"tool_use","usage":{"input_tokens":3,"output_tokens":9},
               "content":[{"type":"tool_use","id":"toolu_live","name":"Read",
                           "input":{"file_path":"a"}}]}}),
        ms(2),
    );
    let (records, _) = live(&writer);
    assert_eq!(
        spans(&records, "af.turn").len(),
        1,
        "the message continues its turn"
    );
    let continued = events(&records, "af.turn.continued");
    assert_eq!(continued.len(), 1);
    assert_eq!(continued[0].span_id, turns[0].span_id);
    assert_eq!(continued[0].attributes["af.turn.tool_calls"], 1);
    assert_eq!(continued[0].attributes["af.usage.output_tokens"], "9");
    assert_eq!(
        continued[0].attributes["gen_ai.response.finish_reasons"],
        json!(["tool_use"])
    );
    let tools = spans(&records, "af.tool");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].parent_span_id.as_deref(),
        Some(turns[0].span_id.as_str())
    );
    assert_eq!(tools[0].start_unix_ns, ms(2).to_string());
    // The tool results go back: the tool ends and the next turn opens at that line.
    send(
        &mut tap,
        json!({"type":"user","message":{"content":[
               {"type":"tool_result","tool_use_id":"toolu_live","content":"x"}]}}),
        ms(3),
    );
    let (records, _) = live(&writer);
    let turns = spans(&records, "af.turn");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].start_unix_ns, ms(3).to_string());
    assert_eq!(
        end_of(&records, &tools[0].span_id).end_unix_ns.as_deref(),
        Some(ms(3).to_string().as_str())
    );
    send(
        &mut tap,
        json!({"type":"assistant","message":{"id":"msg_live_two","stop_reason":"end_turn",
               "content":[{"type":"text","text":"done"}]}}),
        ms(4),
    );
    // The final message ends the second turn before the result line arrives.
    let (records, _) = live(&writer);
    assert_eq!(
        end_of(&records, &turns[1].span_id).end_unix_ns.as_deref(),
        Some(ms(4).to_string().as_str())
    );
    send(
        &mut tap,
        json!({"type":"result","subtype":"success","is_error":false,"result":"done"}),
        ms(5),
    );
    tap.finish(&writer, &release(offset, 0, false));
    let (records, _) = live(&writer);
    assert_eq!(
        spans(&records, "af.turn").len(),
        2,
        "the result opens no turn"
    );
    let ends = records
        .iter()
        .filter(|record| matches!(record, Record::SpanEnd(end) if end.span_id == turns[1].span_id))
        .count();
    assert_eq!(ends, 1, "each turn ends once");
    let ids = &events(&records, "af.harness.ids")[0].attributes;
    assert_eq!(ids["af.stream.turns"], 2);
    assert_eq!(ids["af.stream.tool_calls"], 1);
}

#[test]
fn telemetry_codex_items_become_spans() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, options) = harness(dir.path(), Level::Spans, Harness::Codex);
    let mut tap = Tap::new(&writer, options);
    let lines: Vec<String> = [
        json!({"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}),
        json!({"type":"turn.started"}),
        json!({"type":"item.started","item":{"id":"item_0","type":"command_execution",
               "command":"bash -lc ls","aggregated_output":"","status":"in_progress"}}),
        json!({"type":"item.completed","item":{"id":"item_0","type":"command_execution",
               "command":"bash -lc ls","aggregated_output":"Cargo.toml\nsrc\n","exit_code":0,
               "status":"completed"}}),
        json!({"type":"item.completed","item":{"id":"item_1","type":"reasoning",
               "text":"private reasoning words"}}),
        json!({"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call",
               "server":"docs","tool":"search","arguments":{"q":"x"},"status":"failed",
               "error":{"message":"no such server"}}}),
        json!({"type":"item.completed","item":{"id":"item_3","type":"agent_message",
               "text":"final answer words"}}),
        json!({"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":200,
               "output_tokens":30,"reasoning_output_tokens":12}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect();
    let start = 1_700_000_000_000_000_000;
    let bytes = feed(&mut tap, &writer, Stream::Stdout, &lines, start);
    tap.finish(&writer, &release(bytes, 0, false));
    let (records, text) = live(&writer);
    for words in [
        "private reasoning",
        "final answer",
        "Cargo.toml",
        "no such server",
    ] {
        assert!(!text.contains(words), "{words} is untrusted text");
    }
    let turns = spans(&records, "af.turn");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].start_unix_ns, (start + 1_000_000).to_string());
    let turn = end_of(&records, &turns[0].span_id);
    assert_eq!(
        turn.end_unix_ns.as_deref(),
        Some((start + 7_000_000).to_string().as_str())
    );
    assert_eq!(turn.attributes["af.usage.input_tokens"], "1200");
    assert_eq!(turn.attributes["af.usage.cache_read_tokens"], "200");
    assert_eq!(turn.attributes["af.usage.reasoning_tokens"], "12");
    assert_eq!(turn.attributes["gen_ai.usage.input_tokens"], "1200");
    assert_eq!(turn.attributes["af.usage.observation"], "complete");
    let tools = spans(&records, "af.tool");
    assert_eq!(tools.len(), 2, "a command and an MCP call");
    assert_eq!(tools[0].attributes["gen_ai.tool.name"], "command_execution");
    assert_eq!(tools[0].start_unix_ns, (start + 2_000_000).to_string());
    let command = end_of(&records, &tools[0].span_id);
    assert_eq!(
        command.end_unix_ns.as_deref(),
        Some((start + 3_000_000).to_string().as_str())
    );
    assert_eq!(command.attributes["af.tool.exit_code"], 0);
    assert_eq!(command.attributes["af.tool.output_bytes"], 15);
    assert_eq!(command.attributes["af.outcome"], "succeeded");
    let mcp = end_of(&records, &tools[1].span_id);
    assert_eq!(mcp.attributes["af.tool.status"], "failed");
    assert_eq!(mcp.attributes["af.outcome"], "failed");
    assert_eq!(
        events(&records, "af.item.reasoning")[0].attributes["af.item.text_bytes"],
        23
    );
    assert_eq!(
        events(&records, "af.item.agent_message")[0].attributes["af.item.text_bytes"],
        18
    );
    let init = &events(&records, "af.harness.init")[0].attributes;
    assert_eq!(
        init["gen_ai.conversation.id"],
        corr(
            &writer.segment_id().unwrap(),
            "0199a213-81c0-7800-8aa1-bbab2a035a53"
        )
    );
}

#[test]
fn telemetry_frame_over_bound_withheld_whole() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, mut options) = harness(dir.path(), Level::Content, Harness::Claude);
    options.frame_max_bytes = 256;
    let mut tap = Tap::new(&writer, options);
    // A credential that begins inside the bound and ends past it, on a line split in chunks.
    let credential = format!("sk-proj-{}", "Q7v".repeat(40));
    let line = format!(
        "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{}{}\"}}]}}}}\n",
        "p".repeat(200),
        credential
    );
    let next = "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed\"}}\n";
    let bytes = line.as_bytes();
    let mut offset = 0;
    for piece in [&bytes[..120], &bytes[120..230], &bytes[230..]] {
        tap.chunk(&writer, Stream::Stdout, offset, piece, 1);
        offset += piece.len() as u64;
    }
    tap.chunk(&writer, Stream::Stdout, offset, next.as_bytes(), 2);
    offset += next.len() as u64;
    let released = tap.finish(&writer, &release(offset, 0, false));
    let (records, text) = live(&writer);
    assert!(!text.contains("sk-proj"), "no prefix of the frame anywhere");
    assert!(!text.contains("pppp"));
    let withheld = events(&records, "af.stream.frame_withheld");
    assert_eq!(withheld.len(), 1);
    assert_eq!(withheld[0].attributes["af.stream.frame.bytes"], line.len());
    assert_eq!(withheld[0].attributes["af.stream"], "stdout");
    // Nothing of it was parsed into a turn, nothing of it pended; the next line was read.
    assert!(spans(&records, "af.turn").is_empty());
    assert_eq!(events(&records, "af.harness.rate_limit").len(), 1);
    assert!(released.contents.iter().all(|content| {
        !String::from_utf8_lossy(&content.bytes).contains("sk-proj")
            && !String::from_utf8_lossy(&content.bytes).contains("ppp")
    }));
    assert_eq!(
        events(&records, "af.harness.ids")[0].attributes["af.stream.analysis"],
        "incomplete"
    );
}

#[test]
fn telemetry_observer_gap_never_joins_fragments() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, options) = harness(dir.path(), Level::Spans, Harness::Claude);
    let mut tap = Tap::new(&writer, options);
    // Two lines whose head and tail, with the middle dropped, would form a valid `assistant`
    // message the harness never wrote.
    let first =
        "{\"type\":\"assistant\",\"message\":{\"id\":\"msg_joined\",\"content\":[]},\"x\":\"";
    let middle = "dropped by the observer\"}\n{\"type\":\"user\",\"y\":\"";
    let last = "\"}\n";
    let after = "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"rejected\"}}\n";
    let joined: Value =
        serde_json::from_str(&format!("{first}{}", last.trim_end())).expect("valid if joined");
    assert_eq!(joined["type"], "assistant");
    tap.chunk(&writer, Stream::Stdout, 0, first.as_bytes(), 10);
    let gap_from = first.len() as u64;
    let gap_to = gap_from + middle.len() as u64;
    // The middle chunk never arrives: the next offset is past it.
    tap.chunk(&writer, Stream::Stdout, gap_to, last.as_bytes(), 20);
    let after_at = gap_to + last.len() as u64;
    tap.chunk(&writer, Stream::Stdout, after_at, after.as_bytes(), 30);
    tap.finish(&writer, &release(after_at + after.len() as u64, 0, false));
    let (records, _) = live(&writer);
    let gaps = events(&records, "af.stream.gap");
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].attributes["af.stream.gap.from"], gap_from);
    assert_eq!(gaps[0].attributes["af.stream.gap.to"], gap_to);
    assert_eq!(gaps[0].time_unix_ns, "20");
    assert!(
        spans(&records, "af.turn").is_empty(),
        "no message was joined"
    );
    assert_eq!(
        events(&records, "af.harness.rate_limit")[0].attributes["af.rate_limit.status"],
        "rejected",
        "framing resumes after the first newline past the gap"
    );
    let ids = &events(&records, "af.harness.ids")[0].attributes;
    assert_eq!(ids["af.stream.gaps"], 1);
    assert_eq!(ids["af.stream.analysis"], "incomplete");
    // A stream shorter than the supervisor collected ends in a trailing gap.
    let dir = tempfile::tempdir().unwrap();
    let (writer, options) = harness(dir.path(), Level::Spans, Harness::Claude);
    let mut tap = Tap::new(&writer, options);
    tap.chunk(&writer, Stream::Stdout, 0, b"{\"type\":\"user\"}\n", 5);
    tap.finish(&writer, &release(100, 7, false));
    let (records, _) = live(&writer);
    let gaps = events(&records, "af.stream.gap");
    assert_eq!(gaps.len(), 2, "both streams end short");
    assert_eq!(gaps[0].attributes["af.stream.gap.from"], 16);
    assert_eq!(gaps[0].attributes["af.stream.gap.to"], 100);
    assert_eq!(gaps[1].attributes["af.stream"], "stderr");
}

#[test]
fn telemetry_observer_redacts_split_credential() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, options) = harness(dir.path(), Level::Content, Harness::Codex);
    let mut tap = Tap::new(&writer, options);
    let line = json!({"type":"item.completed","item":{"id":"item_0","type":"agent_message",
                      "text":"configured from /grants/codex-home-value/config.toml"}})
    .to_string()
        + "\n";
    let split = line.find("codex-home").unwrap() + 5;
    tap.chunk(&writer, Stream::Stdout, 0, &line.as_bytes()[..split], 1);
    tap.chunk(
        &writer,
        Stream::Stdout,
        split as u64,
        &line.as_bytes()[split..],
        2,
    );
    let stderr = b"token=0123456789abcdefghij\n";
    tap.chunk(&writer, Stream::Stderr, 0, stderr, 3);
    let released = tap.finish(
        &writer,
        &release(line.len() as u64, stderr.len() as u64, false),
    );
    let message = released
        .contents
        .iter()
        .find(|content| content.kind == "agent_message")
        .unwrap();
    let text = String::from_utf8(message.bytes.clone()).unwrap();
    assert_eq!(text, "configured from <grant:CODEX_HOME>/config.toml");
    let line = released
        .contents
        .iter()
        .find(|content| content.kind == "line")
        .unwrap();
    assert_eq!(
        String::from_utf8(line.bytes.clone()).unwrap(),
        review_telemetry::redact::WITHHELD_LINE
    );
    let (_, segment) = live(&writer);
    assert!(!segment.contains("codex-home-value"));
    assert!(!segment.contains("0123456789abcdefghij"));
}

#[test]
fn telemetry_stream_withheld_until_decision() {
    // A device code on one line and the challenge URL on the next; the same wrapped across
    // lines; and the two split across stdout and stderr: each withholds the whole capture.
    let code = b"Enter the code WXYZ-1234 when asked\n".as_slice();
    let url = b"open https://claude.ai/oauth/authorize?code=true\n".as_slice();
    let wrapped_url = b"open https://claude.ai/oau\nth/authorize?code=true\n".as_slice();
    for (stdout, stderr) in [
        ([code, url].concat(), Vec::new()),
        ([code, wrapped_url].concat(), Vec::new()),
        (code.to_vec(), url.to_vec()),
        (
            b"device_code: WXYZ-1234\n".to_vec(),
            b"verification_uri: elsewhere\n".to_vec(),
        ),
    ] {
        let decision = Release::decide(&stdout, &stderr, &[], false, false);
        assert!(decision.withheld && decision.native_withheld);
    }
    let clean = Release::decide(b"{\"type\":\"result\"}\n", b"", &[], false, false);
    assert!(!clean.withheld);
    assert!(
        Release::decide(b"", b"", &[], true, false).withheld,
        "the adapter replaced it"
    );
    assert!(
        Release::decide(b"", b"", &[], false, true).withheld,
        "an authentication failure"
    );
    assert!(
        Release::decide(b"", b"", &[b"user_code ABCD".as_slice()], false, false).withheld,
        "pending native content"
    );
    // At `content`, nothing of a withheld process's content or identifiers is handed on, and
    // nothing of it was ever live.
    let dir = tempfile::tempdir().unwrap();
    let (writer, options) = harness(dir.path(), Level::Content, Harness::Claude);
    let mut tap = Tap::new(&writer, options);
    let stdout = [
        json!({"type":"system","subtype":"init","session_id":"session-withheld"}).to_string() + "\n",
        json!({"type":"assistant","message":{"id":"msg_withheld","content":[{"type":"text","text":"Enter the code WXYZ-1234"}]}}).to_string() + "\n",
    ]
    .concat();
    tap.chunk(&writer, Stream::Stdout, 0, stdout.as_bytes(), 1);
    tap.chunk(&writer, Stream::Stderr, 0, url, 2);
    let decision = Release::decide(stdout.as_bytes(), url, &[], false, false);
    let released = tap.finish(
        &writer,
        &decision.with_sizes(stdout.len() as u64, url.len() as u64),
    );
    assert_eq!(
        released,
        Released {
            withheld: true,
            withheld_ids: 2,
            ..Released::default()
        }
    );
    let (records, text) = live(&writer);
    for words in ["WXYZ", "claude.ai", "session-withheld", "msg_withheld"] {
        assert!(!text.contains(words), "{words}");
    }
    let ids = &events(&records, "af.harness.ids")[0].attributes;
    assert_eq!(ids["af.stream.withheld"], true);
    assert_eq!(ids["af.native.withheld"], true);
    assert_eq!(ids["af.harness.ids.count"], 2);
    assert!(ids.get("af.harness.ids").is_none());
    assert!(events(&records, "af.harness.ids.part").is_empty());
}

#[test]
fn telemetry_harness_ids_never_published_live() {
    let token = format!("sk-ant-{}", "Ab9".repeat(12));
    let challenge = "https://claude.ai/oauth/authorize?code=1".to_string();
    let plain = "msg_01PlainIdentifier".to_string();
    for kind in [Harness::Claude, Harness::Codex] {
        let dir = tempfile::tempdir().unwrap();
        let (writer, options) = harness(dir.path(), Level::Spans, kind);
        let mut tap = Tap::new(&writer, options);
        let lines: Vec<String> = match kind {
            Harness::Claude => vec![
                json!({"type":"system","subtype":"init","session_id":token}),
                json!({"type":"assistant","message":{"id":challenge,"content":[
                    {"type":"tool_use","id":plain,"name":"Read","input":{}},
                    {"type":"tool_use","id":token,"name":"Read","input":{}}]}}),
                json!({"type":"user","message":{"content":[
                    {"type":"tool_result","tool_use_id":plain,"content":"x"},
                    {"type":"tool_result","tool_use_id":token,"content":"y"}]}}),
                json!({"type":"permission_denied","tool_name":"Bash","tool_use_id":challenge}),
                json!({"type":"result","subtype":"success","session_id":challenge}),
            ],
            Harness::Codex => vec![
                json!({"type":"thread.started","thread_id":token}),
                json!({"type":"turn.started"}),
                json!({"type":"item.started","item":{"id":challenge,"type":"command_execution","command":"ls"}}),
                json!({"type":"item.completed","item":{"id":challenge,"type":"command_execution","exit_code":0,"status":"completed"}}),
                json!({"type":"item.completed","item":{"id":plain,"type":"agent_message","text":"ok"}}),
                json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}),
            ],
        }
        .iter()
        .map(Value::to_string)
        .collect();
        let bytes = feed(&mut tap, &writer, Stream::Stdout, &lines, 1);
        // Before the decision only correlation ids are live.
        let (_, before) = live(&writer);
        for original in [&token, &challenge, &plain] {
            assert!(!before.contains(original.as_str()), "{kind:?}: {original}");
        }
        assert!(before.contains(&corr(&writer.segment_id().unwrap(), &token)));
        let released = tap.finish(&writer, &release(bytes, 0, false));
        let (records, after) = live(&writer);
        // After it, only the originals that pass the credential rules, in the ids event alone.
        assert!(!after.contains(token.as_str()), "{kind:?}");
        assert!(!after.contains("claude.ai"), "{kind:?}");
        let ids = &events(&records, "af.harness.ids")[0].attributes;
        let listed = ids["af.harness.ids"].as_array().unwrap();
        let segment = writer.segment_id().unwrap();
        assert!(
            listed.contains(&json!(format!("{} {plain}", corr(&segment, &plain)))),
            "{kind:?}: {listed:?}"
        );
        assert_eq!(released.withheld_ids, 2, "{kind:?}");
        assert_eq!(ids["af.harness.ids.withheld"], 2, "{kind:?}");
        // Every identifier admitted, counted before the release drained them: the token, the
        // challenge and the plain one.
        assert_eq!(ids["af.harness.ids.count"], 3, "{kind:?}");
        assert_eq!(ids["af.harness.ids.parts"], 0, "{kind:?}");
        assert_eq!(after.matches(plain.as_str()).count(), 1, "{kind:?}");
    }
}

/// A process with more distinct harness identifiers than one record's array holds (more than
/// 32, and more bytes than one array takes) keeps every admitted mapping: the `af.harness.ids`
/// event lists the first part and says how many `af.harness.ids.part` events follow it, each
/// within the record bounds and cut nowhere, and `af.harness.ids.count` counts them all.
#[test]
fn telemetry_harness_ids_beyond_one_record_are_all_kept() {
    // Short identifiers fill a part's 32 elements; long ones its bytes first.
    for (count, filler, parts_expected) in [(40, 0, 1), (100, 120, 3)] {
        let dir = tempfile::tempdir().unwrap();
        let (writer, options) = harness(dir.path(), Level::Spans, Harness::Codex);
        let mut tap = Tap::new(&writer, options);
        let originals: Vec<String> = (0..count)
            .map(|index| format!("item_{index:03}_{}", "x".repeat(filler)))
            .collect();
        let mut lines = vec![json!({"type":"turn.started"}).to_string()];
        lines.extend(originals.iter().map(|id| {
            json!({"type":"item.completed","item":{"id":id,"type":"agent_message","text":"ok"}})
                .to_string()
        }));
        lines.push(
            json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}})
                .to_string(),
        );
        let bytes = feed(&mut tap, &writer, Stream::Stdout, &lines, 1);
        let released = tap.finish(&writer, &release(bytes, 0, false));
        assert_eq!(released.ids.len(), originals.len());
        let (records, _) = live(&writer);
        let segment = writer.segment_id().unwrap();
        let ids = &events(&records, "af.harness.ids")[0].attributes;
        assert_eq!(ids["af.harness.ids.count"], originals.len());
        assert_eq!(ids["af.harness.ids.withheld"], 0);
        assert_eq!(ids["af.harness.ids.omitted"], 0);
        assert_eq!(ids["af.harness.ids.analysis"], "complete");
        let parts = events(&records, "af.harness.ids.part");
        assert_eq!(parts.len(), parts_expected, "{count}");
        assert_eq!(ids["af.harness.ids.parts"], parts.len());
        if filler == 0 {
            assert_eq!(ids["af.harness.ids"].as_array().unwrap().len(), 32);
        }
        let mut listed = Vec::new();
        for (ordinal, attributes) in std::iter::once(ids)
            .chain(parts.iter().map(|part| &part.attributes))
            .enumerate()
        {
            if ordinal > 0 {
                assert_eq!(attributes["af.harness.ids.part"], ordinal);
            }
            assert!(
                attributes.get("af.truncated_fields").is_none(),
                "{attributes:?}"
            );
            let part = attributes["af.harness.ids"].as_array().unwrap();
            assert!(!part.is_empty() && part.len() <= 32, "{}", part.len());
            listed.extend(part.iter().map(|text| text.as_str().unwrap().to_string()));
        }
        let expected: Vec<String> = originals
            .iter()
            .map(|original| format!("{} {original}", corr(&segment, original)))
            .collect();
        assert_eq!(listed, expected, "{count}");
    }
}

/// An identifier past the bounds of the pending mappings — the 4097th distinct one of a
/// process, or one longer than 256 bytes — keeps its correlation id in the live records but can
/// have no mapping: `af.harness.ids.omitted` counts each such identifier once and
/// `af.harness.ids.analysis` says the identifier analysis is incomplete, while every mapping
/// within the bounds is listed.
#[test]
fn telemetry_harness_ids_past_their_bounds_are_counted() {
    let at_bound = format!("item_{}", "x".repeat(251));
    let past_bound = format!("item_{}", "x".repeat(252));
    assert_eq!((at_bound.len(), past_bound.len()), (256, 257));
    let many: Vec<String> = (0..4097).map(|index| format!("item_{index:04}")).collect();
    for (name, originals, admitted) in [
        ("count", many.clone(), 4096),
        ("length", vec![at_bound.clone(), past_bound.clone()], 1),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (writer, options) = harness(dir.path(), Level::Spans, Harness::Codex);
        let mut tap = Tap::new(&writer, options);
        let item = |id: &String| {
            json!({"type":"item.completed","item":{"id":id,"type":"agent_message","text":"ok"}})
                .to_string()
        };
        let mut lines = vec![json!({"type":"turn.started"}).to_string()];
        lines.extend(originals.iter().map(item));
        // The omitted identifier again: counted once.
        lines.push(item(originals.last().unwrap()));
        lines.push(
            json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}})
                .to_string(),
        );
        let bytes = feed(&mut tap, &writer, Stream::Stdout, &lines, 1);
        let released = tap.finish(&writer, &release(bytes, 0, false));
        assert_eq!(released.ids.len(), admitted, "{name}");
        let (records, text) = live(&writer);
        let segment = writer.segment_id().unwrap();
        let omitted = corr(&segment, originals.last().unwrap());
        // The omitted identifier's correlation id is live; its original is nowhere.
        assert!(text.contains(&omitted), "{name}");
        assert!(!text.contains(originals.last().unwrap().as_str()), "{name}");
        let ids = &events(&records, "af.harness.ids")[0].attributes;
        assert_eq!(ids["af.harness.ids.count"], admitted, "{name}");
        assert_eq!(ids["af.harness.ids.omitted"], 1, "{name}");
        assert_eq!(ids["af.harness.ids.analysis"], "incomplete", "{name}");
        let parts = events(&records, "af.harness.ids.part");
        assert_eq!(ids["af.harness.ids.parts"], parts.len(), "{name}");
        let listed: Vec<String> = std::iter::once(ids)
            .chain(parts.iter().map(|part| &part.attributes))
            .flat_map(|attributes| attributes["af.harness.ids"].as_array().unwrap().clone())
            .map(|text| text.as_str().unwrap().to_string())
            .collect();
        let expected: Vec<String> = originals[..admitted]
            .iter()
            .map(|original| format!("{} {original}", corr(&segment, original)))
            .collect();
        assert_eq!(listed, expected, "{name}");
    }
}

#[test]
fn pending_content_is_bounded_by_the_attempt_allowance_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, mut options) = harness(dir.path(), Level::Content, Harness::Codex);
    options.content_allowance_bytes = 40;
    let mut tap = Tap::new(&writer, options);
    let lines: Vec<String> = (0..4)
        .map(|index| {
            json!({"type":"item.completed","item":{"id":format!("item_{index}"),"type":"agent_message",
                   "text":format!("message number {index} ...")}})
            .to_string()
        })
        .collect();
    let bytes = feed(&mut tap, &writer, Stream::Stdout, &lines, 1);
    let released = tap.finish(&writer, &release(bytes, 0, false));
    let kept: Vec<String> = released
        .contents
        .iter()
        .map(|content| String::from_utf8(content.bytes.clone()).unwrap())
        .collect();
    assert_eq!(kept, ["message number 2 ...", "message number 3 ..."]);
    assert_eq!(released.dropped_contents, 2);
    assert!(
        released
            .contents
            .iter()
            .all(|content| content.media == Media::Text)
    );
}

/// Package P7: output blobs are the assistant's text blocks, so the Claude `result` line that
/// repeats the final text is recorded by size and structure only and pends nothing. With an
/// allowance that fits the thinking block and the text exactly, the repeated result evicts
/// neither: both are released and published, nothing is dropped.
#[test]
fn telemetry_claude_result_never_displaces_stream_content() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, mut options) = harness(dir.path(), Level::Content, Harness::Claude);
    let thinking = "weighing the two readings of the spec";
    let answer = "the final answer, repeated by the result";
    options.content_allowance_bytes = (thinking.len() + answer.len()) as u64;
    let mut tap = Tap::new(&writer, options);
    let lines: Vec<String> = [
        json!({"type":"system","subtype":"init","session_id":"session-result"}),
        json!({"type":"assistant","message":{"id":"msg_result","model":"claude-fixture-1",
               "stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2},
               "content":[{"type":"thinking","thinking":thinking},
                          {"type":"text","text":answer}]}}),
        json!({"type":"result","subtype":"success","is_error":false,"result":answer,
               "session_id":"session-result"}),
    ]
    .iter()
    .map(Value::to_string)
    .collect();
    let bytes = feed(&mut tap, &writer, Stream::Stdout, &lines, 1);
    let released = tap.finish(&writer, &release(bytes, 0, false));
    let kept: Vec<(&str, String)> = released
        .contents
        .iter()
        .map(|content| {
            (
                content.kind,
                String::from_utf8(content.bytes.clone()).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kept,
        [
            ("thinking", thinking.to_string()),
            ("text", answer.to_string())
        ]
    );
    assert_eq!(released.dropped_contents, 0);
    assert_eq!(released.published, 2);
    let (records, _) = live(&writer);
    assert_eq!(events(&records, "gen_ai.content.output").len(), 1);
    assert_eq!(events(&records, "af.content.reasoning").len(), 1);
    let result = &events(&records, "af.harness.result")[0].attributes;
    assert_eq!(result["af.result.bytes"], answer.len());
    let ids = &events(&records, "af.harness.ids")[0].attributes;
    assert_eq!(ids["af.content.dropped"], 0);
    assert_eq!(ids["af.content.published"], 2);
}

/// Design §7.3: at the seal every stream queue is drained, and what a queue still holds then —
/// its tap never got its decision, or its chunks outran the bound — counts in the seal's
/// `dropped_chunks`, with every chunk its observer dropped.
#[test]
fn chunks_still_queued_at_the_seal_count_as_dropped() {
    struct Stuck;
    impl review_telemetry::segment::writer::StreamQueue for Stuck {
        fn drain(&self, _: &Writer) -> bool {
            true
        }
        fn has_pending(&self) -> bool {
            false
        }
        fn finished(&self) -> bool {
            false
        }
        fn discard(&self) -> u64 {
            5
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let writer = crate::writer(dir.path(), Level::Spans);
    writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    writer.attach_stream(std::sync::Arc::new(Stuck));
    writer.count_dropped_chunks(2);
    let sealed = writer
        .finish(review_telemetry::record::SealExit::Ok)
        .unwrap()
        .unwrap();
    let seal = crate::sealed_records(&sealed.path)
        .into_iter()
        .find_map(|record| match record {
            Record::Seal(seal) => Some(seal),
            _ => None,
        })
        .unwrap();
    assert_eq!(seal.dropped_chunks, Some(7));
}
