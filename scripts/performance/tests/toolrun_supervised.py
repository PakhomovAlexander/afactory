"""Diagnostic tool runs under the real authority supervisor (ADR-0175); test code only.

`crates/review-process/tests/diagnostic_tools.rs` builds the `perf-supervisor` example and runs
this script with its path. Each case runs one `perflab.toolrun` request through it and prints
one JSON line of what came back, which the Rust test judges: the supervisor's deadline, stdin,
drain, descendant and cancellation semantics are the ones verdict samples get, never a weaker
copy in the driver. The last cases run the diagnostic driver's own checks over its replies: a
stream whose retained prefix parses as JSON, and a step whose files together outgrow its room
(ADR-0176).
"""
import json
import os
import sys
import tempfile
import time

LAB = os.path.join(sys.argv[1], "scripts", "performance")
sys.path.insert(0, LAB)
sys.dont_write_bytecode = True

from perflab import diagnostics, limits, perfcapture, toolrun  # noqa: E402

SUPERVISOR = toolrun.Supervisor(sys.argv[2], cancel_margin=0, grace_ms=200)
ENV = {"PATH": "/usr/bin:/bin", "LC_ALL": "C"}
SLACK_SECONDS = 2.0


def report(name, result, started, deadline=None, marker=None):
    now = time.monotonic()
    survived = None
    if marker is not None:
        # The descendant would leave this file 1 s after it started, had it survived.
        time.sleep(2)
        survived = os.path.exists(marker)
    reply = result.supervised.result if result.supervised is not None else None
    print(json.dumps({
        "status": reply["lifecycle"]["status"] if reply else None,
        "survived": survived,
        "case": name, "ok": result.ok, "failure": result.failure(), "code": result.code,
        "timed_out": result.timed_out, "late": result.late, "cancelled": result.cancelled,
        "lifecycle": result.lifecycle, "uncertain": result.uncertain,
        "stdout_retained": len(result.stdout), "stdout_total": result.stdout_bytes,
        "elapsed": now - started,
        "within_deadline": deadline is None or now <= deadline + SLACK_SECONDS}), flush=True)


def case(name, argv, seconds, cap=4096, stdin=b"", budget=None, marker=False):
    cwd = tempfile.mkdtemp()
    started = time.monotonic()
    deadline = None if budget is None else started + budget
    result = toolrun.run(SUPERVISOR, argv, ENV, seconds, cap, stdin=stdin, cwd=cwd,
                         deadline=deadline)
    report(name, result, started, deadline, os.path.join(cwd, "survived") if marker else None)


case("plain", ["/bin/sh", "-c", "echo hi"], 10)
# A child that never reads its stdin, given a whole record of it, under a short deadline.
case("blocked_stdin", ["/bin/sh", "-c", "exec sleep 5"], 30, stdin=b"x" * 60000,
     budget=SUPERVISOR.reserve() + 0.6)
# A child that exits 0 at once while its descendant holds the inherited stdout and stderr.
case("inherited_pipes", ["/bin/sh", "-c", "(sleep 1; touch survived) & exit 0"], 30,
     budget=SUPERVISOR.reserve() + 1.0, marker=True)
# A child that exits 0 while a descendant lives on, its pipes closed.
case("live_descendant", ["/bin/sh", "-c",
                         "(sleep 1; touch survived) </dev/null >/dev/null 2>&1 & exit 0"], 30,
     budget=SUPERVISOR.reserve() + 1.0, marker=True)
# More stdout than the cap: counted, never parsed as if whole.
case("partial_stream", ["/bin/sh", "-c", "yes | head -c 200000"], 10, cap=1000)
# Cancelled through the supervisor while it runs.
started = time.monotonic()
running = toolrun.start(SUPERVISOR, ["/bin/sh", "-c", "exec sleep 30"], ENV, 60, 4096,
                        cwd=tempfile.mkdtemp())
time.sleep(0.5)
running.cancel()
report("cancelled", running.wait(), started)
# A probe-sized stream of 65546 bytes whose 65536-byte retained prefix is one valid padded
# JSON line: the prefix parses, but the stream was longer, so the run is no success (root-3).
line = json.dumps({"schema": "af.perf-probe/1"}).encode()
record = line + b" " * (limits.RECORD_BYTES_MAX - len(line) - 1) + b"\n"
stream = tempfile.mkdtemp()
with open(os.path.join(stream, "out"), "wb") as out:
    out.write(record + b"x" * 10)
started = time.monotonic()
result = toolrun.run(SUPERVISOR, ["/bin/cat", os.path.join(stream, "out")], ENV, 10,
                     limits.RECORD_BYTES_MAX, cwd=stream)
try:
    parses = json.loads(result.stdout.decode("utf-8")) is not None
except ValueError:
    parses = False
report("parsing_prefix", result, started)
print(json.dumps({"case": "parsing_prefix_detail", "prefix_parses": parses,
                  "supervision_failure": result.supervision_failure(),
                  "within_deadline": True, "uncertain": result.uncertain}), flush=True)


def aggregate_profile(root):
    """The diagnostic driver's own step bounds over the real supervisor: a 32-byte cap whose
    evidence room is 16 bytes (root-4). The lifecycle state a step reads is the constructor's:
    no scratch kept for recovery yet, no stage in trusted local mode, and the run's work below
    its output."""
    profile = diagnostics.Profile.__new__(diagnostics.Profile)
    profile.output = root
    os.makedirs(os.path.join(root, "out"))
    profile.budget = diagnostics.Budget({"total_minutes": 15, "profile_bytes": 32,
                                         "builds": 2, "tool_output_bytes": 1024,
                                         "tool_seconds": 30}, root)
    profile.clock = time.monotonic
    profile.supervisor = SUPERVISOR
    profile.execution = {"mode": diagnostics.TRUSTED_LOCAL}
    profile.retained = []
    profile.work = os.path.join(root, diagnostics.WORK_DIR)
    profile.stage = None
    return profile


def aggregate(name, writer, retained=()):
    profile = aggregate_profile(os.path.join(tempfile.mkdtemp(), "run"))
    profile.retained.extend(retained)
    started = time.monotonic()
    try:
        profile.tool_output("out/stdout", ["/bin/sh", "-c", writer], name)
        refused = None
    except (diagnostics.Exhausted, perfcapture.CaptureError) as error:
        refused = error.reason
    record = profile.budget.record(1)["bytes"]
    print(json.dumps({"case": name, "refused": refused, "evidence_used": record["evidence_used"],
                      "evidence_overage": record["evidence_overage"],
                      "elapsed": time.monotonic() - started, "within_deadline": True,
                      "uncertain": False}), flush=True)


aggregate("aggregate_control", "printf abc")
aggregate("aggregate_files", "head -c 16 /dev/zero > one; head -c 16 /dev/zero > two; "
          "printf abc")
aggregate("aggregate_running", "i=0; while [ $i -lt 2000 ]; do printf abcd > f$i; "
          "i=$((i+1)); sleep 0.01; done")
# Scratch an earlier execution kept for recovery halts the run: the step is never started.
aggregate("aggregate_halted", "printf abc", retained=["/kept/for/recovery"])
