"""Stand-in perf, inferno, valgrind, samply and probe executables for the diagnostic tests.

Every one is a small Python program written into a test's own directory, never product code
and never a measurement: each child run they produce is synthetic. They speak exactly the
protocols the laboratory relies on, so the driver's own logic is what the tests exercise:

- The probe performs the `perf-control` region handshake when its channel is named, after a
  configurable slow setup, delivers a message that passes the golden oracle for its instance on
  the channel, and runs each further repetition as a fresh execution of itself.
  It writes its current phase (`setup`, `handshake`, `region`, `after`) to a file beside itself;
  a real probe is blocked in the handshake's read then, so it is never a region sample.
- perf's `record` opens its control FIFO read-write and its acknowledgement FIFO for writing,
  as perf 6.1 does, answers `enable` and `disable` with `ack\\n\\0`, and while enabled records one
  sample a millisecond whose stack names the probe's phase at that instant: a capture that
  enabled during setup would show `setup_work`. Its recording is a little-endian perf.data
  holding declared LOST and LOST_SAMPLES records; `script` and `buildid-list` print their text
  formats from it.
- inferno's three tools collapse, diff and render; the renderer writes the `onload` handler and
  `<script>` element the real `--no-javascript` output still carries.
- `af` answers `--version` and `task show ... --json`, the latter with a document of 10,000
  history entries, far larger than any retained log; it refuses to run without a private HOME,
  XDG and TMPDIR, and exports a phase trace when asked. perf without control FIFOs records the
  whole process. The probe also writes the CLI Task state seed as `setup.sh` expects.
- `docker` runs each `docker run` request on this host: every path the request names is bound at
  its own path, so the stand-in executes the bound supervisor directly and reports what the
  PID-1 supervisor of a real container reports (its namespace scanned, the cgroup limits its
  flags set). It isolates nothing, and no test claims containment from it.
"""
import json
import os
import sys

PROBE = r'''
import json, os, sys, time
here = os.path.dirname(os.path.abspath(sys.argv[0]))
config = json.load(open(os.path.join(here, "fake-probe.json")))
request = json.loads(sys.stdin.read())
phase = os.path.join(here, "phase")
def mark(name):
    # Replaced whole, so a reader never sees it half written.
    with open(phase + ".next", "w") as stream:
        stream.write(name)
    os.replace(phase + ".next", phase)
def line(stream):
    out = b""
    while not out.endswith(b"\n"):
        out += stream.read(1)
    return out
if request["contract_id"] in config.get("refuse", []):
    sys.exit(2)
if request["contract_id"] == "cli_state_fixture_v1":
    # The CLI Task state seed setup.sh writes through the side's probe: its state below
    # probe-work and a message whose check reports exactly 10,000 events.
    os.makedirs(os.path.join(request["scratch"], "probe-work"))
    open(os.path.join(request["scratch"], "probe-work", "events.sqlite"), "w").close()
    sys.stdout.write('{"schema":"af.perf-probe/1","correctness":{"status":"passed",'
                     '"count":10000,"detail":"fake cli seed"}}\n')
    sys.exit(0)
if request["contract_id"].endswith("_seed_v1"):
    # A seed writer writes below its own work directory, as the real probe does.
    os.mkdir(os.path.join(request["scratch"], "probe-work"))
    open(os.path.join(request["scratch"], "probe-work", "written-seed"), "w").close()
    if config.get("seed_symlink"):
        # A writer that leaves a symlink to a file outside its seed beside a passing message.
        os.symlink(config["seed_symlink"], os.path.join(request["scratch"], "probe-work",
                                                        "escape"))
names = ("AF_PERF_REGION_READY", "AF_PERF_REGION_GO", "AF_PERF_REGION_REQUEST")
handshake = all(os.environ.get(name) for name in names)
ready, go = os.environ.get(names[0]), os.environ.get(names[1])
# Each repetition is a fresh execution: one more mark per execution beside this file.
with open(os.path.join(here, "executions"), "a") as stream:
    stream.write("%d\n" % os.getpid())
mark("setup")
# Fresh declared state: the scratch is new and empty for every repetition.
used = os.path.join(request["scratch"], "used-by-a-repetition")
if os.listdir(request["scratch"]) and not request["contract_id"].endswith("_seed_v1"):
    sys.stderr.write("stale scratch\n"); sys.exit(4)
if request["seed"] is not None:
    # Reads the private copy of what its side's seed writer wrote.
    if not os.path.isfile(os.path.join(request["seed"], "written-seed")):
        sys.stderr.write("the seed copy is not the written seed\n"); sys.exit(5)
    if os.path.exists(os.path.join(request["seed"], "touched")):
        sys.stderr.write("a stale seed copy\n"); sys.exit(5)
    open(os.path.join(request["seed"], "touched"), "w").close()
open(used, "w").close()
time.sleep(config.get("setup_seconds", 0))
if config.get("exit_before_region"):
    sys.exit(3)
if handshake and config.get("fail_before_region"):
    # A driven probe that fails in its setup delivers its failure message, never `begin`.
    failed = b'{"failed":"before its region"}\n'
    with open(ready, "wb", buffering=0) as stream:
        stream.write(b"record %d\n" % len(failed) + failed)
    sys.exit(1)
out = answer = None
if handshake:
    mark("handshake")
    out = open(ready, "wb", buffering=0)
    out.write(b"begin\n")
    answer = open(go, "rb", buffering=0)
    assert line(answer) == b"go\n"
mark("region")
started = time.time()
while time.time() - started < config.get("region_seconds", 0.05):
    pass
elapsed = "%d" % max(1, int((time.time() - started) * 1000))
if handshake:
    mark("handshake")
    out.write(b"end\n")
    assert line(answer) == b"go\n"
mark("after")
trace = os.environ.get("AF_PERF_TRACE_OUTPUT")
if trace and config.get("trace"):
    with open(trace, "x") as stream:
        # Padded with trailing whitespace to an exact size when asked; still one document.
        text = json.dumps(config["trace"])
        stream.write(text + " " * max(0, config.get("trace_bytes", 0) - len(text)))
if trace and config.get("trace_symlink"):
    os.symlink(config["trace_symlink"], trace)
if trace and config.get("trace_fifo"):
    os.mkfifo(trace)
entry = config["golden"][request["instance_id"]]
wall = config.get("wall_ms", {}).get("trace" if trace else "plain", elapsed)
message = {"schema": "af.perf-probe/1", "case_id": request["case_id"],
           "instance_id": request["instance_id"], "contract_id": request["contract_id"],
           "fixture_digest": entry["fixture_digest"], "parameters": request["parameters"],
           "metrics": {"wall_ms": {"value": wall, "unit": "ms"},
                       "setup_ms": {"value": "2", "unit": "ms"}},
           "correctness": {"status": "passed", "digest": entry["digest"],
                           "count": entry["count"], "detail": "fake probe"}}
data = (json.dumps(message, sort_keys=True, separators=(",", ":")) + "\n").encode()
if not handshake:
    total = config.get("stdout_total")
    if total:
        # One valid JSON line padded to the 64-KiB record, then the rest of `total` bytes: a
        # retained prefix that parses, of a stream that was longer.
        record = 64 * 1024
        data = data[:-1] + b" " * (record - len(data)) + b"\n"
        data += b"x" * (total - record)
    sys.stdout.buffer.write(data)
    sys.exit(0)
# A driven probe delivers its message on its channel, never on stdout.
out.write(b"record %d\n" % len(data) + data)
out.write(b"next\n")
if line(answer) == b"again\n":
    if config.get("stay_warm"):
        # A probe that repeats in its warm process instead of a fresh execution.
        sys.stderr.write("warm repetition\n"); sys.exit(6)
    request_file = os.open(os.environ["AF_PERF_REGION_REQUEST"], os.O_RDONLY)
    os.dup2(request_file, 0)
    os.execv(sys.executable, [sys.executable, "-I", os.path.abspath(sys.argv[0])])
'''

PERF = r'''
import json, os, select, struct, subprocess, sys, time
here = os.path.dirname(os.path.abspath(sys.argv[0]))
config = json.load(open(os.path.join(here, "fake-perf.json")))
args = sys.argv[1:]
if args == ["--version"]:
    print("perf version 6.1.fake"); sys.exit(0)
def option(name):
    return args[args.index(name) + 1]
def write_data(path, payload, lost, lost_samples):
    # A little-endian perf.data: its header, then the samples as records of a type no reader
    # knows, then the declared loss records, each with its payload.
    records = b""
    body = json.dumps(payload).encode()
    for start in range(0, len(body), 60000):
        chunk = body[start:start + 60000]
        records += struct.pack("<IHH", 99, 0, 8 + len(chunk)) + chunk
    for count in lost:
        records += struct.pack("<IHHQQ", 2, 0, 24, 7, count)
    for count in lost_samples:
        records += struct.pack("<IHHQ", 13, 0, 16, count)
    header = b"PERFILE2" + struct.pack("<QQ", 104, 136) + struct.pack("<QQ", 104, 0)
    header += struct.pack("<QQ", 104, len(records)) + struct.pack("<QQ", 0, 0) + bytes(32)
    with open(path, "xb") as stream:
        stream.write(header + records)
def read_data(path):
    data = open(path, "rb").read()
    offset, size = struct.unpack_from("<QQ", data, 40)
    position, body = offset, b""
    while position < offset + size:
        kind, _, length = struct.unpack_from("<IHH", data, position)
        if kind == 99:
            body += data[position + 8:position + length]
        position += length
    return json.loads(body)
if args[0] == "record":
    output = option("-o")
    controls = [item for item in args if item.startswith("--control=fifo:")]
    command = args[args.index("--") + 1:]
    sizes = [item for item in args if item.startswith("--max-size=")]
    assert all(item.endswith("K") for item in sizes), sizes
    if controls:
        ctl_path, ack_path = controls[0][len("--control=fifo:"):].split(",")
        ctl = os.open(ctl_path, os.O_RDWR | os.O_NONBLOCK)
        ack = os.open(ack_path, os.O_WRONLY)
    else:
        # The whole process: recording from its start, no region to enable.
        ctl = os.open("/dev/null", os.O_RDONLY)
    child = subprocess.Popen(command)
    enabled, samples, buffer = not controls, [], b""
    phase_file = config.get("phase_file")
    while child.poll() is None:
        ready, _, _ = select.select([ctl], [], [], 0.001)
        if ready:
            buffer += os.read(ctl, 64)
            while b"\n" in buffer:
                line, buffer = buffer.split(b"\n", 1)
                if line in (b"enable", b"disable"):
                    enabled = line == b"enable"
                    if not config.get("never_ack"):
                        os.write(ack, b"ack\n\x00")
        if enabled and not controls:
            samples.append(config.get("whole_phase", "af"))
            time.sleep(0.001)
        elif enabled:
            phase = "unknown"
            if phase_file and os.path.exists(phase_file):
                phase = open(phase_file).read().strip() or "unknown"
            if not config.get("no_samples"):
                samples.append(phase)
            if sizes and len(samples) >= config.get("size_limit_samples", 1 << 30):
                # perf 6.1 ends its session at --max-size and terminates the workload.
                sys.stderr.write("[ perf record: perf size limit reached (261 KB), stopping "
                                 "session ]\n")
                child.terminate()
                child.wait()
                break
    write_data(output, {"samples": samples, "unwind": option("--call-graph"),
                        "event": option("-e")},
               config.get("lost", []), config.get("lost_samples", []))
    if config.get("pad_bytes"):
        with open(output, "ab") as stream:
            stream.write(b"\0" * config["pad_bytes"])
    sys.exit(child.returncode if child.returncode >= 0 else 128 - child.returncode)
data = read_data(option("-i"))
def option_event(data):
    return data["event"]
if args[0] == "script":
    for index, phase in enumerate(data["samples"]):
        sys.stdout.write("perf-probe 4242/4242 %d.%06d: %s:\n" % (
            index // 1000, index % 1000, config.get("recorded_event", option_event(data))))
        sys.stdout.write("\t    55aa00000010 leaf_work (/fake/perf-probe)\n")
        sys.stdout.write("\t    55aa00000020 %s_work (/fake/perf-probe)\n" % phase)
        if data["unwind"] == "fp":
            for _ in range(config.get("unknown_with_frame_pointers", 0)):
                sys.stdout.write("\t    7f0000000030 [unknown] ([unknown])\n")
        sys.stdout.write("\t    55aa00000040 main (/fake/perf-probe)\n\n")
    sys.exit(0)
if args[0] == "buildid-list":
    print("0123456789abcdef0123456789abcdef01234567 /fake/perf-probe")
    sys.exit(0)
sys.exit(64)
'''

COLLAPSE = r'''
import sys
counts, stack = {}, []
def flush():
    if stack:
        key = ";".join(reversed(stack))
        counts[key] = counts.get(key, 0) + 1
for line in open(sys.argv[1]).read().splitlines():
    if not line.strip():
        flush(); stack = []
    elif line[0].isspace():
        rest = line.strip().split(" ", 1)[1]
        stack.append(rest.rsplit(" (", 1)[0])
flush()
for key in sorted(counts):
    print("%s %d" % (key, counts[key]))
'''

DIFF = r'''
import sys
args = [arg for arg in sys.argv[1:] if not arg.startswith("--")]
assert "--normalize" in sys.argv
def read(path):
    out = {}
    for line in open(path).read().splitlines():
        stack, count = line.rsplit(" ", 1)
        out[stack] = int(count)
    return out
before, after = read(args[0]), read(args[1])
for stack in sorted(set(after)):
    print("%s %d %d" % (stack, before.get(stack, 0), after[stack]))
'''

FLAMEGRAPH = r'''
import sys
args = sys.argv[1:]
assert "--deterministic" in args and "--no-javascript" in args
title = args[args.index("--title") + 1]
rows = [line for line in open(args[-1]).read().splitlines() if line]
out = ['<?xml version="1.0" standalone="no"?>',
       '<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">',
       '<svg version="1.1" width="1200" height="200" onload="init(evt)" viewBox="0 0 1200 200" '
       'xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" '
       'xmlns:fg="http://github.com/jonhoo/inferno">',
       '<!--Flame graph stack visualization.-->',
       '<defs><linearGradient id="background" y1="0" y2="1" x1="0" x2="0">'
       '<stop stop-color="#eeeeee" offset="5%"/><stop stop-color="#eeeeb0" offset="95%"/>'
       '</linearGradient></defs>',
       '<style type="text/css">text { font-family:monospace; font-size:12px }</style>',
       '<script type="text/ecmascript"><![CDATA[var nametype = "Function:";]]></script>',
       '<rect x="0" y="0" width="100%" height="200" fill="url(#background)"/>',
       '<text id="title" fill="rgb(0,0,0)" x="50.0000%%" y="24.00">%s%s</text>'
       % (title.replace("&", "&amp;").replace("<", "&lt;"), " (negated)" if "--negate" in args else ""),
       '<svg id="frames" x="10" width="1180" total_samples="%d"><g>' % len(rows)]
for index, row in enumerate(rows):
    stack = row.rsplit(" ", 2 if row.count(" ") >= 2 and row.rsplit(" ", 2)[1].isdigit() else 1)[0]
    name = stack.split(";")[-1].replace("&", "&amp;").replace("<", "&lt;")
    out.append('<g><title>%s</title><rect x="0.0000%%" y="%d" width="100.0000%%" height="15" '
               'fill="rgb(230,100,50)" fg:x="0" fg:w="1"/><text x="0.25%%" y="%d.50">%s</text></g>'
               % (name, 150 - 16 * index, 160 - 16 * index, name))
out.append('</g></svg></svg>')
print("\n".join(out))
'''

VALGRIND = r'''
import os, re, subprocess, sys
args = sys.argv[1:]
if args == ["--version"]:
    print("valgrind-3.fake"); sys.exit(0)
command = args[args.index("--") + 1:]
tool = [arg for arg in args if arg.startswith("--tool=")][0][len("--tool="):]
if tool == "callgrind":
    out = [arg for arg in args if arg.startswith("--callgrind-out-file=")][0].split("=", 1)[1]
    child = subprocess.Popen(command)
    code = child.wait()
    here = os.path.dirname(os.path.abspath(sys.argv[0]))
    switched = os.path.join(here, "switched-any")
    instructions = 0
    if os.path.exists(switched):
        instructions = 1000 * open(switched).read().count("on")
    with open(out, "w") as stream:
        stream.write("version: 1\ncreator: callgrind-fake\nevents: Ir\nsummary: %d\ntotals: %d\n"
                     % (instructions, instructions))
    sys.exit(code)
if tool == "dhat":
    child = subprocess.Popen(command)
    code = child.wait()
    sys.stderr.write("==1== Total:     12,345 bytes in 67 blocks\n")
    sys.exit(code)
sys.exit(64)
'''

CALLGRIND_CONTROL = r'''
import os, sys
args = sys.argv[1:]
if args == ["--version"]:
    print("Callgrind_control-3.fake"); sys.exit(0)
assert args[0] == "-i" and args[1] in ("on", "off"), args
here = os.path.dirname(os.path.abspath(sys.argv[0]))
# The pid named is valgrind's own process; its guest is its child here.
with open(os.path.join(here, "switched-any"), "a") as stream:
    stream.write(args[1] + "\n")
'''


AF = r'''
import json, os, sys, time
here = os.path.dirname(os.path.abspath(sys.argv[0]))
config = json.load(open(os.path.join(here, "fake-af.json")))
args = sys.argv[1:]
if os.environ.get("AF_SELF_OFFLINE") != "1":
    sys.stderr.write("af: diagnostic setup and samples must be offline\n"); sys.exit(10)
home = os.environ.get("HOME", "")
# Its own private HOME, XDG directories and TMPDIR: one directory made for this execution.
private = home and home != config.get("real_home") and all(
    os.environ.get(name) == home for name in ("XDG_CONFIG_HOME", "XDG_STATE_HOME",
                                              "XDG_DATA_HOME", "XDG_CACHE_HOME", "TMPDIR"))
if not private:
    sys.stderr.write("af: no private home\n"); sys.exit(9)
with open(os.path.join(here, "af-executions"), "a") as stream:
    stream.write(json.dumps(args) + "\n")
started = time.time()
while time.time() - started < config.get("busy_seconds", 0.02):
    pass
trace = os.environ.get("AF_PERF_TRACE_OUTPUT")
if trace:
    if config.get("fill_recorder"):
        # Startup filled the recorder: its spans are kept and the command's was dropped.
        spans = [{"id": 1, "parent": None, "phase": "cli_startup", "tid": 1, "start_ns": 10,
                  "end_ns": 900}]
        dropped = 3
    else:
        spans = [{"id": 1, "parent": None, "phase": "cli_startup", "tid": 1, "start_ns": 10,
                  "end_ns": 400}]
        if args[:1] != ["--version"]:
            spans.append({"id": 2, "parent": None, "phase": "cli_command", "tid": 1,
                          "start_ns": 400, "end_ns": 900})
        dropped = 0
    text = json.dumps({"schema": "af.perf-trace/1", "pid": os.getpid(),
                       "clock": "monotonic_ns", "epoch_unix_ns": "1", "capacity": 65536,
                       "dropped": dropped, "open": 0, "spans": spans})
    with open(trace, "x") as stream:
        # Padded with trailing whitespace to an exact size when asked; still one document.
        stream.write(text + " " * max(0, config.get("trace_bytes", 0) - len(text)))
    if config.get("linger_seconds"):
        # Still live after its trace is exported: a bounded wait, then its finished marker.
        time.sleep(config["linger_seconds"])
        with open(os.path.join(here, "af-lingered"), "a") as stream:
            stream.write("finished\n")
if args == ["--version"]:
    print("af " + config.get("version", "0.12.0-rc.3"))
    sys.exit(0)
if args[:2] == ["task", "show"]:
    state = args[args.index("--state") + 1]
    if not os.path.isfile(os.path.join(state, "events.sqlite")):
        sys.stderr.write("af: no Task state\n"); sys.exit(3)
    skip = config.get("skip_sequence")
    out = sys.stdout
    out.write('{"task_id":"perf-probe-task","history":[')
    first = True
    for number in range(10000):
        if number == skip:
            continue
        kind = "lease_renewed" if 2 <= number <= 9998 else "phase_changed"
        out.write(("" if first else ",") + '{"sequence":%d,"transition":{"change":{"kind":'
                  '"%s"}},"note":"%s"}' % (number, kind, "x" * 140))
        first = False
    out.write(']}\n')
    sys.exit(0)
sys.exit(64)
'''

DOCKER = r'''
import json, os, resource, shutil, signal, subprocess, sys
here = os.path.dirname(os.path.abspath(sys.argv[0]))
args = sys.argv[1:]
with open(os.path.join(here, "docker-calls.jsonl"), "a") as log:
    log.write(json.dumps(args) + "\n")
if args[0] in ("rm", "ps"):
    sys.exit(0)
assert args[0] == "run", args
mounts = {}
for index, item in enumerate(args):
    if item == "--mount":
        fields = dict(part.split("=", 1) if "=" in part else (part, "")
                      for part in args[index + 1].split(","))
        mounts[fields["dst"]] = fields["src"]
supervisor = mounts[args[args.index("--entrypoint") + 1]]
limit = None
if "--ulimit" in args:
    limit = int(args[args.index("--ulimit") + 1].split("=")[1].split(":")[0])


def limited():
    if limit is not None:
        resource.setrlimit(resource.RLIMIT_FSIZE, (limit, limit))


request = sys.stdin.buffer.read()
child = subprocess.Popen([supervisor], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         preexec_fn=limited)
signal.signal(signal.SIGTERM, lambda signum, frame: child.send_signal(signal.SIGTERM))
out, _ = child.communicate(request)
if child.returncode != 0 or b"\n" not in out:
    sys.stdout.buffer.write(out)
    sys.exit(child.returncode or 1)
line, rest = out.split(b"\n", 1)
result = json.loads(line)


def scanned(record):
    exited = record["termination"]["kind"] in ("exited", "signalled") and \
        record.get("exit_observation", record.get("timing", {}).get("exit_observation")) \
        == "unreaped"
    record["lifecycle"] = {"observation": "pid_namespace_scan",
                           "descendants_at_exit": 0 if exited else None,
                           "remaining_after_cleanup": 0, "pipes_held": False,
                           "status": "clean", "detail": "stand-in container"}


scanned(result)
if result["setup"] is not None:
    scanned(result["setup"])
result["supervisor"]["namespace_init"] = True
# The supervisor clears its scratch: the per-sample directory bound at /perf/scratch.
sample = mounts["/perf/scratch"]
for name in os.listdir(sample):
    path = os.path.join(sample, name)
    if os.path.isdir(path) and not os.path.islink(path):
        shutil.rmtree(path)
    else:
        os.unlink(path)
result["scratch"] = {"status": "cleared", "detail": ""}
memory = args[args.index("--memory") + 1]
result["observations"] = {"before": {
    "cgroup/memory.max": {"value": memory}, "cgroup/memory.swap.max": {"value": "0"},
    "cgroup/pids.max": {"value": args[args.index("--pids-limit") + 1]},
    "cgroup/cpu.max": {"value": "max 100000"}}, "after": {}, "unavailable": None}
sys.stdout.buffer.write(json.dumps(result).encode() + b"\n" + rest)
'''


def af(directory, **config):
    """A stand-in AF binary; `config` sets its version, busy_seconds, skip_sequence,
    fill_recorder, trace_bytes (the exported trace padded to that size), linger_seconds (a
    bounded wait after the trace is exported, then the `af-lingered` marker) and the real_home
    it must not run in."""
    path = os.path.join(directory, "af")
    os.makedirs(directory, exist_ok=True)
    with open(os.path.join(directory, "fake-af.json"), "w") as stream:
        json.dump(config, stream)
    with open(path, "w") as stream:
        stream.write("#!%s -I\n%s" % (sys.executable, AF))
    os.chmod(path, 0o755)
    return path


def docker(directory):
    path = os.path.join(directory, "docker")
    os.makedirs(directory, exist_ok=True)
    with open(path, "w") as stream:
        stream.write("#!%s -I\n%s" % (sys.executable, DOCKER))
    os.chmod(path, 0o755)
    return path


def docker_calls(directory):
    with open(os.path.join(directory, "docker-calls.jsonl")) as stream:
        return [json.loads(line) for line in stream]


def probe(directory, golden, **config):
    """A stand-in probe passing the oracle for every golden instance; `config` sets its
    setup_seconds, region_seconds, exit_before_region, trace (padded to trace_bytes) and
    wall_ms."""
    entries = dict((entry["instance_id"], entry)
                   for entry in golden["entries"] + golden["seeds"])
    path = os.path.join(directory, "perf-probe")
    os.makedirs(directory, exist_ok=True)
    with open(os.path.join(directory, "fake-probe.json"), "w") as stream:
        json.dump(dict(config, golden=entries), stream)
    with open(path, "w") as stream:
        stream.write("#!%s -I\n%s" % (sys.executable, PROBE))
    os.chmod(path, 0o755)
    return path


def perf(directory, **config):
    path = os.path.join(directory, "perf")
    os.makedirs(directory, exist_ok=True)
    with open(os.path.join(directory, "fake-perf.json"), "w") as stream:
        json.dump(config, stream)
    with open(path, "w") as stream:
        stream.write("#!%s -I\n%s" % (sys.executable, PERF))
    os.chmod(path, 0o755)
    return path


def inferno(directory):
    os.makedirs(directory, exist_ok=True)
    for name, body in (("inferno-collapse-perf", COLLAPSE), ("inferno-diff-folded", DIFF),
                       ("inferno-flamegraph", FLAMEGRAPH)):
        path = os.path.join(directory, name)
        with open(path, "w") as stream:
            stream.write("#!%s -I\n%s" % (sys.executable, body))
        os.chmod(path, 0o755)
    return directory


def valgrind(directory):
    os.makedirs(directory, exist_ok=True)
    for name, body in (("valgrind", VALGRIND), ("callgrind_control", CALLGRIND_CONTROL)):
        path = os.path.join(directory, name)
        with open(path, "w") as stream:
            stream.write("#!%s -I\n%s" % (sys.executable, body))
        os.chmod(path, 0o755)
    return directory
