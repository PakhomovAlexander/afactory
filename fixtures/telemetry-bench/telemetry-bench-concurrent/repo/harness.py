"""The scripted harness of the telemetry bench fixtures (fixtures/telemetry-bench/README.md).

A command Worker the Task runtime supervises exactly as it supervises a Provider's model
process (`ModelRunner::capture_filtered`). It reads its request, writes stream-json-shaped
`assistant` lines to stderr, and answers a clean reviewer result on stdout. `bench.json` beside
it says how: `paced` writes `lines` lines of `line_bytes` bytes, one every `interval_ms`, then
answers; `dense` answers first, then writes `bytes` bytes of lines at pipe speed, and its last
write is one line naming the unix nanosecond it was made at, after which it closes both streams
and exits at once. Credential-free; never touches the network.
"""

import json
import os
import sys
import time

REPLY = (
    b'{"schema":"af.worker-reply/1","outputs":{"result":[{"reports":[],'
    b'"benchmark_demands":[],"dispositions":[]}]}}\n'
)
WORDS = b"the harness reports progress on the captured source and keeps going "


def line(name, index, width):
    """One `assistant` line of exactly `width` bytes, its newline included."""
    head = (
        '{"type":"assistant","message":{"id":"msg_bench_%s_%08d","model":"bench",'
        '"stop_reason":null,"content":[{"type":"text","text":"' % (name, index)
    ).encode()
    tail = b'"}]}}\n'
    filler = max(0, width - len(head) - len(tail))
    return head + (WORDS * (filler // len(WORDS) + 1))[:filler] + tail


def write_all(fd, data):
    view = memoryview(data)
    while view:
        written = os.write(fd, view)
        view = view[written:]


def main():
    name = sys.argv[1]
    sys.stdin.buffer.read()
    with open("bench.json", encoding="utf-8") as file:
        harness = json.load(file)["harness"]
    width = harness["line_bytes"]
    if harness["mode"] == "paced":
        for index in range(harness["lines"]):
            write_all(2, line(name, index, width))
            time.sleep(harness["interval_ms"] / 1000)
        write_all(1, REPLY)
        return
    write_all(1, REPLY)
    remaining = harness["bytes"]
    batch = bytearray()
    index = 0
    while remaining > 0:
        piece = line(name, index, min(width, remaining))
        index += 1
        remaining -= len(piece)
        batch += piece
        if len(batch) >= 1 << 20:
            write_all(2, batch)
            batch = bytearray()
    if batch:
        write_all(2, batch)
    last = time.time_ns()
    write_all(
        2,
        b'{"type":"result","subtype":"af_bench_last_write","last_write_unix_ns":"%d"}\n'
        % last,
    )
    os.close(1)
    os.close(2)
    os._exit(0)


main()
