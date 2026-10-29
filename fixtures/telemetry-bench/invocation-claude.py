#!/usr/bin/python3
"""The scripted Claude harness of the invocation-cost fixtures (fixtures/telemetry-bench/README.md).

Installed as `claude` on the measured command's `PATH`, with the Provider registry naming its
auth directory. It answers af's token-free checks (the help page, the version, the login status,
the model catalog session) and the Provider admission probe, and runs one scripted Attempt per
business request: `bench.json` in its auth directory (`CLAUDE_CONFIG_DIR`, written by the
measurement for each run, so the repository and its Snapshot are the same for both shapes) names
the `shape` it prints and the bytes of intermediate tool output. Under either shape it builds the
same tool output, chunk by chunk, and answers the same final result; under `json` it prints
only the result envelope, under `stream-json` every message as a line — init, one assistant
`tool_use` and one user `tool_result` per chunk, the final assistant message, then the very same
result line. Its last write is one stderr line naming the unix nanosecond it was made at, the
same moment on the filesystem clock (`last_write_fs_ns`) and the bytes it wrote to stdout
(unless `report` is false). The `af` integration tests use the shape `requested`, which prints
the shape af asked for, and these optional keys, all absent in the measurements: `record_stdin` (a file name in the auth directory the request bytes are written
to, as received), `thinking` (texts printed as `thinking` blocks under `stream-json`: the first
in the first assistant message, the rest in the final one, in order), `say` (a text block
printed before the final reply under `stream-json`) and `tool_output_prefix` (text that begins
the first tool result).
Credential-free; never touches the network.
"""

import json
import os
import sys
import tempfile
import time

HELP = """Usage: claude [options] [command] [prompt]

Options:
  --allowedTools, --allowed-tools <tools...>  Comma or space-separated list of tool names to allow
  --effort <level>                      Effort level for the current session
  -h, --help                            Display help for command
  --json-schema <schema>                JSON Schema for structured output
  --model <model>                       Model for the current session
  --output-format <format>              Output format (only works with --print)
  --permission-mode <mode>              Permission mode to use for the session
  -p, --print                           Print response and exit
  --restricted                          Restricted mode
  --safe-mode                           Start with all customizations disabled
  --strict-mcp-config                   Only use MCP servers from --mcp-config
  --tools <tools...>                    Specify the list of available tools
  --verbose                             Override verbose mode setting from config
  -v, --version                         Output the version number
"""
MODEL = 'claude-fixture-1'
USAGE = {'input_tokens': 10, 'output_tokens': 2, 'cache_creation_input_tokens': 0,
         'cache_read_input_tokens': 0}
REPLY = {'schema': 'af.worker-reply/1',
         'outputs': {'result': [{'reports': [], 'benchmark_demands': [], 'dispositions': []}]}}
WORDS = 'the tool reports what it read from the captured source and keeps going '


def file_clock_ns():
    """The filesystem clock now: the modification time a file written at this moment gets.

    The drain's end is the drained result's modification time in the Store, a reading of the
    kernel's file timestamp clock, which can trail `time.time_ns()` by up to a tick. A start
    taken from that same clock, after every earlier write, orders the two readings.
    """
    with tempfile.TemporaryFile() as marker:
        marker.write(b'.')
        marker.flush()
        return os.fstat(marker.fileno()).st_mtime_ns


def write(fd, data):
    view = memoryview(data)
    while view:
        view = view[os.write(fd, view):]


def line(message):
    return (json.dumps(message, separators=(',', ':')) + '\n').encode()


def envelope(result, structured):
    message = {'type': 'result', 'subtype': 'success', 'is_error': False, 'duration_ms': 1,
               'num_turns': 1, 'result': result, 'session_id': '00000000-0000-4000-8000-000000000001',
               'usage': USAGE}
    if structured is not None:
        message['structured_output'] = structured
    return line(message)


def main():
    args = sys.argv[1:]
    if args == ['--help']:
        sys.stdout.write(HELP)
        return
    if args == ['--version']:
        print('2.1.303 (Claude Code)')
        return
    if args[:2] == ['auth', 'status']:
        print(json.dumps({'loggedIn': True, 'apiProvider': 'firstParty', 'authMethod': 'claude.ai',
                          'email': 'bench@example.invalid'}))
        return
    if '--input-format' in args:
        # The model catalog session: no catalog, so bindings plan unverified.
        return
    request = sys.stdin.buffer.read()
    try:
        json.loads(request)
    except ValueError:
        # The Provider admission probe: plain text, answered OK.
        write(1, envelope('OK', None))
        return
    with open(os.path.join(os.environ['CLAUDE_CONFIG_DIR'], 'bench.json'), encoding='utf-8') as file:
        harness = json.load(file)['harness']
    if harness.get('record_stdin'):
        with open(os.path.join(os.environ['CLAUDE_CONFIG_DIR'], harness['record_stdin']), 'wb') as file:
            file.write(request)
    thinking = [{'type': 'thinking', 'thinking': text} for text in harness.get('thinking', [])]
    # The shape is the fixture's, whatever af asked for: the measurement compares what each
    # shape costs af at one level. `requested` prints the shape af asked for (the af fixtures).
    if harness['shape'] == 'requested':
        stream = 'stream-json' in args and '--verbose' in args
    else:
        stream = harness['shape'] == 'stream-json'
    reply = json.dumps(REPLY)
    remaining, chunk, index = harness['tool_output_bytes'], harness['chunk_bytes'], 0
    batch, written = bytearray(), 0
    if stream:
        batch += line({'type': 'system', 'subtype': 'init', 'model': MODEL,
                       'session_id': '00000000-0000-4000-8000-000000000001',
                       'tools': ['Read', 'Glob', 'Grep'], 'mcp_servers': [],
                       'claude_code_version': '2.1.303'})
    while remaining > 0:
        size = min(chunk, remaining)
        remaining -= size
        output = (WORDS * (size // len(WORDS) + 1))[:size]
        if index == 0 and harness.get('tool_output_prefix'):
            output = harness['tool_output_prefix'] + output
        tool = 'toolu_bench_%08d' % index
        content = [{'type': 'tool_use', 'id': tool, 'name': 'Read',
                    'input': {'file_path': 'lib.rs'}}]
        if index == 0 and thinking:
            content = thinking[:1] + content
            thinking = thinking[1:]
        call = line({'type': 'assistant', 'message': {
            'id': 'msg_bench_%08d' % index, 'model': MODEL, 'stop_reason': 'tool_use',
            'usage': USAGE, 'content': content}})
        result = line({'type': 'user', 'message': {'content': [
            {'type': 'tool_result', 'tool_use_id': tool, 'content': output}]}})
        index += 1
        if stream:
            batch += call + result
            if len(batch) >= 1 << 20:
                write(1, batch)
                written += len(batch)
                batch = bytearray()
    if stream:
        said = [{'type': 'text', 'text': harness['say']}] if harness.get('say') else []
        batch += line({'type': 'assistant', 'message': {
            'id': 'msg_bench_final', 'model': MODEL, 'stop_reason': 'end_turn', 'usage': USAGE,
            'content': thinking + said + [{'type': 'text', 'text': reply}]}})
    batch += envelope(reply, REPLY)
    write(1, batch)
    written += len(batch)
    if harness.get('report', True):
        last, last_fs = time.time_ns(), file_clock_ns()
        write(2, b'{"type":"result","subtype":"af_bench_last_write","last_write_unix_ns":"%d",'
              b'"last_write_fs_ns":"%d","stdout_bytes":%d}\n' % (last, last_fs, written))
    os.close(1)
    os.close(2)
    os._exit(0)


main()
