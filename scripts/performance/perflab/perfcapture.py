"""Linux perf capture of exactly a probe's timed region (package p5, ADR-0174, ADR-0175).

**The region.** A library case's setup can last longer than a whole capture, so recording starts
disabled (`perf record --delay=-1`) and the probe's region handshake (`perf-control`) brackets
each region: on the probe's `begin` the driver sends `enable` on perf's control FIFO and waits for
its acknowledgement, then lets the region run; on `end` it sends `disable`, waits again, and lets
the probe finish. Every wait is bounded, an early exit or a refused handshake ends the capture
with its reason, and a capture whose region was never enabled reports it unobserved: a graph of
setup is never presented as one of the region.

**Supervised.** The profiler and the probe it wraps run as one request to the authority
supervisor (`toolrun`), never a process this module starts itself: the supervisor's deadline,
drains, descendant scan and cleanup bound it, and a refused handshake cancels it through the
supervisor (ADR-0175).

**Fresh processes, one recording.** A region far shorter than the sampling period is never
sampled by a new `perf record` per repetition: each new event starts its period afresh (a real
capture of 1,639 such repetitions recorded no sample at all). So one recording spans every
repetition while each repetition is a fresh process image: the probe delivers its message on
the channel (`record N`), asks `next`, and on `again`, which the driver answers once it has
replaced the scratch and seed copy with fresh ones, executes its own image afresh. The event
attached to the task continues across the execution. Each message is judged as it arrives, so
nothing retains an unbounded combined stdout (ADR-0175). The repetitions end when the region has
been recorded for the capture's duration, at the declared repetition cap, the diagnostic
deadline, or when the supervised window could not hold another repetition; the cap's last
region is disabled on time.

**The evidence.** The raw `perf.data` is kept. `perf script` with explicit fields (no period, so
a collapsed weight is one sample) yields the samples, frames and unknown frames (`[unknown]`
symbols, which inferno shows as DSO placeholders, are counted from the script, not guessed from
folded names); the loss records are decoded from the file itself (`decode_losses`), and `perf
buildid-list --with-hits` the build IDs. The effective frequency is samples over the region.
A host-observed command's own time is its exec to its exit in its recording
(`decode_lifetime`), never the wall of the shell that waited at the observer's gate.

**Capability.** `probe_capability` runs a real acknowledged capture of the declared event on a
bounded shell loop and requires decoded samples: a present tool or a zero exit status is not
evidence (an unsupported hardware event exits 0 and counts nothing). Nothing here runs sudo,
changes a sysctl or grants a capability.
"""
import errno
import os
import re
import select
import struct
import time
from fractions import Fraction

from . import limits, toolrun
from .values import sha256_bytes

ACK = b"ack\n"
SCRIPT_FIELDS = "comm,pid,tid,time,event,ip,sym,dso"
CONTROL_COMMANDS = (b"enable\n", b"disable\n")
READY_ENV = "AF_PERF_REGION_READY"
GO_ENV = "AF_PERF_REGION_GO"
REQUEST_ENV = "AF_PERF_REGION_REQUEST"
_FRAME = re.compile(r"^\s+([0-9a-fA-F]+)\s+(.*)$")
_RECORD = re.compile(rb"^record (0|[1-9][0-9]{0,9})\n")
# What may precede a whole `record N\n` line while it is still arriving.
_PARTIAL_RECORD = re.compile(rb"^(?:r|re|rec|reco|recor|record|record |record [0-9]{1,10})$")
# perf 6.1 pads a DSO without a build ID with blanks where its ID would be.
_BUILD_ID = re.compile(r"^([0-9a-f]{16,64})?\s+(\S.*)$")


class CaptureError(Exception):
    """A capture that could not be completed, with the omission reason it records."""

    def __init__(self, reason, detail):
        Exception.__init__(self, detail)
        self.reason = reason
        self.detail = detail


# The control protocol ------------------------------------------------------------------------

def read_ack(fd, deadline, clock=time.monotonic):
    """perf's acknowledgement: `ack\\n`, which perf 6.1 writes with its terminating NUL. Any other
    bytes, end of file or the deadline refuse the handshake."""
    held = b""
    while ACK not in held:
        remaining = deadline - clock()
        if remaining <= 0:
            raise CaptureError("control_timeout", "perf did not acknowledge within its bound "
                               "(received %r)" % held)
        ready, _, _ = select.select([fd], [], [], remaining)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 64)
        except BlockingIOError:
            continue
        if not chunk:
            raise CaptureError("control_closed", "perf closed its acknowledgement FIFO "
                               "(received %r)" % held)
        held += chunk
        if not (ACK.startswith(held[:len(ACK)])):
            raise CaptureError("control_protocol", "perf acknowledged with unexpected bytes %s"
                               % held.hex())
    # Whatever is already there after `ack\n` belongs to it: one NUL at most.
    while True:
        ready, _, _ = select.select([fd], [], [], 0)
        if not ready:
            break
        try:
            chunk = os.read(fd, 64)
        except BlockingIOError:
            break
        if not chunk:
            break
        held += chunk
    if held not in (ACK, ACK + b"\x00"):
        raise CaptureError("control_protocol", "perf acknowledged with unexpected bytes %s"
                           % held.hex())
    return held


class Region(object):
    """What one repetition's handshake observed, in monotonic seconds: `started` is the launch
    for the first repetition and the answer that began it for the others."""

    def __init__(self, started):
        self.started = started
        self.begun = None
        self.enabled = None
        self.disabled = None
        self.ended = None
        self.truncated = False


class Bracketed(object):
    """One profiled recording: its repetitions, the profiler's result and end. `stopped` says
    why it ended: `single` (no repetition was asked for), `declined` (the driver's `on_next`
    asked for no more), `capped` (the recorded interval reached its cap) or `window` (the
    supervised window could not hold another repetition)."""

    def __init__(self, launched):
        self.launched = launched
        self.regions = []
        self.records = 0
        self.exited = None
        self.code = None
        self.stderr = b""
        self.stopped = None
        self.result = None

    def recorded(self):
        return sum(region.disabled - region.enabled for region in self.regions)

    def setup(self):
        return sum(region.begun - region.started for region in self.regions)


class Channel(object):
    """The probe's region channel: its `ready` FIFO, which it writes, and its `go` FIFO, which
    it reads. Both are created new in `directory`."""

    def __init__(self, directory):
        self.ready_path = os.path.join(directory, "ready")
        self.go_path = os.path.join(directory, "go")
        for path in (self.ready_path, self.go_path):
            os.mkfifo(path, 0o600)
        self.ready = os.open(self.ready_path, os.O_RDONLY | os.O_NONBLOCK)
        # Held read-write, so the probe's blocking open for reading finds a writer at once.
        self.go = os.open(self.go_path, os.O_RDWR | os.O_NONBLOCK)
        self.buffer = b""

    def environment(self, request_path):
        return {READY_ENV: self.ready_path, GO_ENV: self.go_path, REQUEST_ENV: request_path}

    def _fill(self, deadline, alive, clock):
        """Read what the probe wrote; False at the deadline or once it ended with nothing
        more to read."""
        remaining = deadline - clock()
        if remaining <= 0:
            return False
        ready, _, _ = select.select([self.ready], [], [], min(remaining, 0.05))
        chunk = b""
        if ready:
            try:
                chunk = os.read(self.ready, 64 * 1024)
            except BlockingIOError:
                chunk = b""
        if chunk:
            self.buffer += chunk
            return True
        if not alive():
            # Whatever it wrote before it ended has been read.
            return False
        if ready:
            # No writer now: the probe is between its executions; it reopens the FIFO.
            time.sleep(0.005)
        return True

    def expect(self, token, deadline, alive, clock=time.monotonic, exit_ends=False):
        """Wait for `token` (`begin\\n`, `end\\n` or `next\\n`): True when it came, False at the
        deadline, or, when `exit_ends`, None once the probe ended without sending anything."""
        while True:
            if self.buffer.startswith(token):
                self.buffer = self.buffer[len(token):]
                return True
            if self.buffer and not token.startswith(self.buffer):
                raise CaptureError("handshake_protocol", "the probe sent %r where %r belongs"
                                   % (self.buffer[:64], token))
            if not self._fill(deadline, alive, clock):
                if clock() >= deadline:
                    return False
                if exit_ends and not self.buffer:
                    return None
                raise CaptureError("probe_exited", "the probe ended before sending %r"
                                   % token)

    def begin_or_record(self, deadline, alive, clock=time.monotonic):
        """Wait for `begin\n`, or for the message record a probe that failed before its region
        sends instead: "begin", "record", or False at the deadline."""
        while True:
            if self.buffer.startswith(b"begin\n"):
                self.buffer = self.buffer[len(b"begin\n"):]
                return "begin"
            if _RECORD.match(self.buffer) is not None:
                return "record"
            if self.buffer and not b"begin\n".startswith(self.buffer) \
                    and _PARTIAL_RECORD.match(self.buffer) is None:
                raise CaptureError("handshake_protocol", "the probe sent %r where its region "
                                   "or its message belongs" % self.buffer[:64])
            if not self._fill(deadline, alive, clock):
                if clock() >= deadline:
                    return False
                raise CaptureError("probe_exited", "the probe ended before its region")

    def record(self, deadline, alive, clock=time.monotonic):
        """One repetition's message, `record N\\n` and exactly N bytes, N at most one 64-KiB
        record; None at the deadline."""
        while True:
            match = _RECORD.match(self.buffer)
            if match is not None:
                size = int(match.group(1))
                if size > limits.RECORD_BYTES_MAX:
                    raise CaptureError("handshake_protocol", "the probe announced a %d-byte "
                                       "message, over its %d-byte record" % (
                                           size, limits.RECORD_BYTES_MAX))
                end = match.end() + size
                if len(self.buffer) >= end:
                    message = self.buffer[match.end():end]
                    self.buffer = self.buffer[end:]
                    return message
            elif self.buffer and _PARTIAL_RECORD.match(self.buffer) is None:
                raise CaptureError("handshake_protocol", "the probe sent %r where its message "
                                   "record belongs" % self.buffer[:64])
            if not self._fill(deadline, alive, clock):
                if clock() >= deadline:
                    return None
                raise CaptureError("probe_exited", "the probe ended before delivering its "
                                   "message")

    def answer(self, token):
        os.write(self.go, token)

    def close(self):
        for fd in (self.ready, self.go):
            try:
                os.close(fd)
            except OSError:
                pass


def _repetitions(run, channel, alive, started, on_begin, on_end, on_record, capture_seconds,
                 run_deadline, window_end, handshake_seconds, on_next, clock):
    """The handshake of every repetition of one bracketed recording, recorded on `run`."""
    while True:
        region = Region(started)
        reached = channel.begin_or_record(run_deadline, alive, clock)
        if not reached:
            raise CaptureError("setup_deadline", "the probe did not reach its region "
                               "before the diagnostic deadline")
        if reached == "record":
            # It failed before its region: its message says why, and is judged.
            on_record(run.records, channel.record(run_deadline, alive, clock) or b"")
            raise CaptureError("probe_failed", "the probe delivered its message without "
                               "reaching its region")
        region.begun = clock()
        on_begin(min(run_deadline, clock() + handshake_seconds))
        region.enabled = clock()
        channel.answer(b"go\n")
        cap = region.enabled + max(capture_seconds - run.recorded(), 0)
        ended = channel.expect(b"end\n", min(cap, run_deadline), alive, clock)
        if not ended:
            # The recorded interval is spent: stop recording now, let the region finish.
            on_end(min(run_deadline, clock() + handshake_seconds))
            region.disabled = clock()
            region.truncated = True
            if not channel.expect(b"end\n", run_deadline, alive, clock):
                raise CaptureError("region_deadline", "the region did not end before the "
                                   "diagnostic deadline")
            region.ended = clock()
        else:
            region.ended = clock()
            on_end(min(run_deadline, clock() + handshake_seconds))
            region.disabled = clock()
        channel.answer(b"go\n")
        run.regions.append(region)
        message = channel.record(run_deadline, alive, clock)
        if message is None:
            raise CaptureError("exit_deadline", "the probe did not deliver its message before "
                               "the diagnostic deadline")
        # Judged as it arrives; a refusal raises and ends the recording.
        on_record(run.records, message)
        run.records += 1
        asked = channel.expect(b"next\n", run_deadline, alive, clock, exit_ends=True)
        if asked is None:
            run.stopped = run.stopped or "single"
            break
        if not asked:
            raise CaptureError("exit_deadline", "the probe did not finish its repetition "
                               "before the diagnostic deadline")
        lasted = clock() - region.started
        if on_next is None:
            run.stopped, more = "single", False
        elif run.recorded() >= capture_seconds:
            run.stopped, more = "capped", False
        elif clock() + 2 * lasted > min(window_end, run_deadline):
            # Another repetition could outlast the supervised window: stop while it is open.
            run.stopped, more = "window", False
        else:
            more = on_next(run)
            run.stopped = None if more else "declined"
        started = clock()
        channel.answer(b"again\n" if more else b"done\n")
        if not more:
            break


def bracketed_run(supervisor, argv, env, cwd, request_path, fifo_dir, on_begin, on_end,
                  on_record, capture_seconds, run_deadline, handshake_seconds, timeout_seconds,
                  on_next=None, file_bytes=None, clock=time.monotonic, watch=None, binds=(),
                  after_start=None):
    """Run `argv` (a profiler wrapping the probe, or the probe itself) once under `supervisor`,
    with the probe's region channel in `fifo_dir` and its request, at `request_path`, as stdin.
    `on_begin(deadline)` and `on_end(deadline)` switch the recording on and off; a region is
    disabled when `capture_seconds` have been recorded in all, even if it is still running.
    `on_record(index, message)` judges each repetition's message as it arrives. After each
    repetition the probe asks for another: `on_next(run)` replaces its state with fresh state
    and returns True for one more, which the probe runs as a fresh execution of its image while
    the recording continues. `file_bytes` bounds every file the recording writes, and `watch`
    all of them together while they are written (`toolrun.Running`): a recording that outgrew
    its room is cancelled and refused as `evidence_over_room`. A contained supervisor binds
    `binds` into the recording's container. `after_start(running)`, when given, is called once
    the run started and before its handshake (the host observer's attachment). Returns a `Bracketed` whose
    profiler exited 0 with a clean lifecycle; anything else raises."""
    channel = Channel(fifo_dir)
    running = None
    try:
        with open(request_path, "rb") as stream:
            stdin = stream.read(limits.RECORD_BYTES_MAX + 1)
        if len(stdin) > limits.RECORD_BYTES_MAX:
            raise CaptureError("probe_refused", "the request is over one 64-KiB record")
        env = dict(env, **channel.environment(request_path))
        launched = clock()
        running = toolrun.start(supervisor, argv, env, timeout_seconds, 4096, stdin=stdin,
                                cwd=cwd, deadline=run_deadline, clock=clock,
                                file_bytes=file_bytes, watch=watch, binds=binds)
        if isinstance(running, toolrun.Result):
            raise CaptureError("probe_failed", running.failure())
        run = Bracketed(launched)
        alive = lambda: not running.done()  # noqa: E731
        try:
            if after_start is not None:
                after_start(running)
            _repetitions(run, channel, alive, launched, on_begin, on_end, on_record,
                         capture_seconds, run_deadline, launched + timeout_seconds,
                         handshake_seconds, on_next, clock)
        except CaptureError as error:
            running.cancel()
            result = running.wait()
            tail = result.stderr.decode("utf-8", "replace").strip()
            error.detail += "; its stderr: %s; %s" % (
                tail[-1024:] or "none", result.failure() or "it exited 0")
            if result.uncertain:
                raise CaptureError("cleanup_uncertain", error.detail)
            if result.exceeded is not None:
                raise CaptureError("evidence_over_room", error.detail)
            raise
        result = running.wait()
        running = None
        run.result = result
        run.exited = clock()
        run.code = result.code
        run.stderr = result.stderr
        failure = result.failure()
        if failure is not None:
            raise CaptureError("cleanup_uncertain" if result.uncertain
                               else "evidence_over_room" if result.exceeded is not None
                               else "probe_failed", failure)
        if result.stdout_bytes:
            raise CaptureError("probe_failed", "the profiled process wrote %d bytes to stdout; "
                               "a driven probe delivers its messages on its channel"
                               % result.stdout_bytes)
        return run
    finally:
        if running is not None and not isinstance(running, toolrun.Result) \
                and not running.done():
            # Anything else that ended the handshake cancels the recording the same way.
            running.cancel()
            if running.wait().uncertain:
                channel.close()
                raise CaptureError("cleanup_uncertain", "the cancelled recording's cleanup is "
                                   "not confirmed")
        channel.close()


class PerfControl(object):
    """perf's control and acknowledgement FIFOs for one `perf record`."""

    def __init__(self, directory):
        self.control_path = os.path.join(directory, "perf-control")
        self.ack_path = os.path.join(directory, "perf-ack")
        os.mkfifo(self.control_path, 0o600)
        os.mkfifo(self.ack_path, 0o600)
        # Opened before perf starts: perf opens the acknowledgement FIFO for writing and blocks
        # until a reader exists, and its control FIFO read-write.
        self.ack = os.open(self.ack_path, os.O_RDONLY | os.O_NONBLOCK)
        self.control = os.open(self.control_path, os.O_RDWR | os.O_NONBLOCK)
        self.acknowledgements = []

    def argument(self):
        return "--control=fifo:%s,%s" % (self.control_path, self.ack_path)

    def send(self, command, deadline):
        if command not in CONTROL_COMMANDS:
            raise ValueError(command)
        try:
            os.write(self.control, command)
        except OSError as error:
            raise CaptureError("control_closed", "cannot send %r to perf: %s"
                               % (command, error))
        self.acknowledgements.append(read_ack(self.ack, deadline))

    def close(self):
        for fd in (self.ack, self.control):
            try:
                os.close(fd)
            except OSError:
                pass


def record_argv(perf, event, frequency, unwind, output, control, command, max_bytes=None):
    """`perf record` of `command` with recording disabled until the region is enabled. With
    `max_bytes`, perf itself stops the session when its output reaches them (in KiB, the unit
    perf 6.1 requires a suffix for), so a DWARF capture cannot outgrow the profile byte cap."""
    # Retain build IDs in perf.data without hard-linking admitted executables into HOME.
    graph = "fp" if unwind == "frame_pointer" else "dwarf,%d" % 32768
    limit = [] if max_bytes is None else ["--max-size=%dK" % max(1, max_bytes // 1024)]
    return [perf, "record", "--no-buildid-cache", "-e", event, "-F", str(frequency),
            "--call-graph", graph, "--delay=-1", control.argument()] + limit + ["-o", output, "--"] + list(command)


def whole_argv(perf, event, frequency, unwind, output, command, max_bytes=None):
    """`perf record` of the whole `command`, recording from its start to its exit: a CLI
    process, whose region is the process itself (ADR-0176). perf exits with the command's own
    status, so its oracle judges it as if it ran alone; `max_bytes` as for `record_argv`."""
    # Retain build IDs in perf.data without hard-linking admitted executables into HOME.
    graph = "fp" if unwind == "frame_pointer" else "dwarf,%d" % 32768
    limit = [] if max_bytes is None else ["--max-size=%dK" % max(1, max_bytes // 1024)]
    return [perf, "record", "--no-buildid-cache", "-e", event, "-F", str(frequency),
            "--call-graph", graph] + limit + ["-o", output, "--"] + list(command)


# perf's text outputs ---------------------------------------------------------------------------

class ScriptStats(object):
    def __init__(self, samples, frames, unknown, events):
        self.samples = samples
        self.frames = frames
        self.unknown = unknown
        # The event names the samples were recorded with, as perf reports them.
        self.events = events

    def ratio(self):
        return Fraction(self.unknown, self.frames) if self.frames else None


def parse_script(text):
    """Count `perf script` samples, frames and unresolved frames. A sample is a header line at
    column zero; its frames are the indented lines below it, `ADDRESS SYMBOL (DSO)`."""
    samples = frames = unknown = 0
    events = set()
    in_sample = False
    for raw in text.splitlines():
        if not raw.strip():
            in_sample = False
            continue
        if not raw[0].isspace():
            samples += 1
            in_sample = True
            # `COMM PID/TID TIME: EVENT: ...`
            parts = raw.split(": ")
            if len(parts) < 2 or not parts[1].strip():
                raise CaptureError("malformed_script", "perf script sample header without its "
                                   "event: %r" % raw[:200])
            events.add(parts[1].strip().rstrip(":"))
            continue
        match = _FRAME.match(raw)
        if not in_sample or match is None:
            raise CaptureError("malformed_script", "perf script line is not a frame: %r"
                               % raw[:200])
        frames += 1
        rest = match.group(2)
        symbol = rest.rsplit(" (", 1)[0] if rest.endswith(")") and " (" in rest else rest
        if symbol.strip() in ("[unknown]", ""):
            unknown += 1
    return ScriptStats(samples, frames, unknown, events)


def check_event(stats, event):
    """perf falls back to another event when the declared one cannot be opened (`cycles:u`
    becomes `cpu-clock:uH` where hardware counters are unsupported) and says so only in a
    warning. A capture of any event but the declared one is refused, never relabelled."""
    if stats.samples and stats.events != {event}:
        raise CaptureError("event_substituted", "perf recorded %s instead of the declared %s; "
                           "no other event is substituted for it"
                           % (", ".join(sorted(stats.events)), event))


# Loss records, decoded from the recording itself ---------------------------------------------

PERF_FILE_MAGIC = b"PERFILE2"
PERF_HEADER_BYTES = 104
PERF_RECORD_LOST = 2
PERF_RECORD_LOST_SAMPLES = 13
PERF_RECORD_AUXTRACE = 71
PERF_RECORD_COMPRESSED = 81
LOSS_KEYS = ("lost_event_records", "lost_events", "lost_sample_records",
             "potentially_lost_samples")


def unknown_losses(detail):
    """Losses that could not be decoded: unknown, never zero."""
    return dict(dict((key, None) for key in LOSS_KEYS), state="unknown", detail=detail[:1024])


def decode_losses(read, size):
    """The loss records of a `perf.data` file of `size` bytes, read through `read(offset,
    count)`, with their payloads kept apart (ADR-0175):

    - `PERF_RECORD_LOST` records (`lost_event_records`) each carry the number of events the
      kernel dropped (`lost_events`);
    - `PERF_RECORD_LOST_SAMPLES` records (`lost_sample_records`) each carry the number of
      samples that were potentially lost (`potentially_lost_samples`).

    A record count is never a payload, and the two payloads have different meanings, so no sum
    of them is reported. Anything this reader cannot decode (another file format or byte
    order, an unfinished header, compressed records, a malformed record) makes all four
    unknown, never zero."""
    header = read(0, PERF_HEADER_BYTES)
    if len(header) < PERF_HEADER_BYTES or header[:8] != PERF_FILE_MAGIC:
        return unknown_losses("not a little-endian perf.data file (no PERFILE2 header)")
    header_size, = struct.unpack_from("<Q", header, 8)
    data_offset, data_size = struct.unpack_from("<QQ", header, 40)
    if header_size != PERF_HEADER_BYTES:
        return unknown_losses("a perf.data header of %d bytes, not %d"
                              % (header_size, PERF_HEADER_BYTES))
    if data_size == 0:
        return unknown_losses("perf did not finish the file: its data section has no size")
    if data_offset < PERF_HEADER_BYTES or data_offset + data_size > size:
        return unknown_losses("the data section [%d, %d) is outside the %d-byte file"
                              % (data_offset, data_offset + data_size, size))
    counts = dict((key, 0) for key in LOSS_KEYS)
    position, end = data_offset, data_offset + data_size
    while position < end:
        head = read(position, 8)
        if len(head) < 8:
            return unknown_losses("a record header at %d is cut short" % position)
        kind, _, length = struct.unpack("<IHH", head)
        if length < 8 or position + length > end:
            return unknown_losses("a record of %d bytes at %d does not fit its data section"
                                  % (length, position))
        step = length
        if kind == PERF_RECORD_COMPRESSED:
            return unknown_losses("the recording holds compressed records, which this reader "
                                  "does not decode")
        if kind == PERF_RECORD_LOST:
            if length < 24:
                return unknown_losses("a LOST record of %d bytes at %d" % (length, position))
            lost, = struct.unpack("<Q", read(position + 16, 8))
            counts["lost_event_records"] += 1
            counts["lost_events"] += lost
        elif kind == PERF_RECORD_LOST_SAMPLES:
            if length < 16:
                return unknown_losses("a LOST_SAMPLES record of %d bytes at %d"
                                      % (length, position))
            lost, = struct.unpack("<Q", read(position + 8, 8))
            counts["lost_sample_records"] += 1
            counts["potentially_lost_samples"] += lost
        elif kind == PERF_RECORD_AUXTRACE:
            if length < 16:
                return unknown_losses("an AUXTRACE record of %d bytes at %d"
                                      % (length, position))
            # Its trace data follows the record itself.
            extra, = struct.unpack("<Q", read(position + 8, 8))
            step += extra
        position += step
    if position != end:
        return unknown_losses("the last record ends at %d, past its data section's end %d"
                              % (position, end))
    return dict(counts, state="decoded", detail="")


PERF_RECORD_COMM = 3
PERF_RECORD_EXIT = 4
PERF_RECORD_MISC_COMM_EXEC = 1 << 13
PERF_SAMPLE_TID = 1 << 1
PERF_SAMPLE_TIME = 1 << 2
# The `sample_id` fields every non-sample record carries with `sample_id_all`, in their order:
# TID, TIME, ID, STREAM_ID, CPU, IDENTIFIER; only TID precedes TIME.
PERF_SAMPLE_ID_FIELDS = (1 << 1) | (1 << 2) | (1 << 6) | (1 << 9) | (1 << 7) | (1 << 16)
PERF_ATTR_SAMPLE_ID_ALL = 1 << 18
PERF_ATTR_BYTES = 48


def _unknown_lifetime(pid, detail):
    return CaptureError("command_boundary_unknown", "the recording does not bound process %d's "
                        "command: %s; its wait at the observer's gate cannot be told apart from "
                        "the command's own time, so none of it is attributed" % (pid, detail))


def decode_lifetime(read, size, pid):
    """(exec, exit) of process `pid` in perf's clock, in nanoseconds, decoded from a `perf.data`
    file of `size` bytes read through `read(offset, count)`: the kernel's `PERF_RECORD_COMM`
    with its exec flag (the gate executing the command in the attached task) and its
    `PERF_RECORD_EXIT`. The interval between them is the command alone, without the observer's
    attachment, the wait at the gate or any Docker client time. A recording this reader cannot
    decode, or one without both records of `pid`, raises `command_boundary_unknown`, never an
    estimate."""
    header = read(0, PERF_HEADER_BYTES)
    if len(header) < PERF_HEADER_BYTES or header[:8] != PERF_FILE_MAGIC:
        raise _unknown_lifetime(pid, "not a little-endian perf.data file")
    header_size, attr_size, attrs_offset, attrs_size, data_offset, data_size = \
        struct.unpack_from("<QQQQQQ", header, 8)
    if header_size != PERF_HEADER_BYTES or data_size == 0 \
            or data_offset < PERF_HEADER_BYTES or data_offset + data_size > size:
        raise _unknown_lifetime(pid, "an unfinished or malformed header")
    if attr_size < PERF_ATTR_BYTES + 16 or not attrs_size or attrs_size % attr_size \
            or attrs_offset + attrs_size > size or attrs_size // attr_size > 64:
        raise _unknown_lifetime(pid, "no readable event attributes")
    kinds = set()
    for offset in range(attrs_offset, attrs_offset + attrs_size, attr_size):
        attr = read(offset, PERF_ATTR_BYTES)
        if len(attr) < PERF_ATTR_BYTES:
            raise _unknown_lifetime(pid, "an event attribute is cut short")
        sample_type, = struct.unpack_from("<Q", attr, 24)
        flags, = struct.unpack_from("<Q", attr, 40)
        if not flags & PERF_ATTR_SAMPLE_ID_ALL or not sample_type & PERF_SAMPLE_TIME:
            raise _unknown_lifetime(pid, "its events carry no time on their task records")
        kinds.add(sample_type & PERF_SAMPLE_ID_FIELDS)
    if len(kinds) != 1:
        raise _unknown_lifetime(pid, "its events disagree on their record identity fields")
    fields = kinds.pop()
    trailer = 8 * bin(fields).count("1")
    time_at = 8 if fields & PERF_SAMPLE_TID else 0
    executed = exited = None
    position, end = data_offset, data_offset + data_size
    while position < end:
        head = read(position, 8)
        if len(head) < 8:
            raise _unknown_lifetime(pid, "a record header at %d is cut short" % position)
        kind, misc, length = struct.unpack("<IHH", head)
        if length < 8 or position + length > end:
            raise _unknown_lifetime(pid, "a record at %d does not fit its data section"
                                    % position)
        step = length
        if kind == PERF_RECORD_COMPRESSED:
            raise _unknown_lifetime(pid, "it holds compressed records")
        if kind == PERF_RECORD_COMM and misc & PERF_RECORD_MISC_COMM_EXEC:
            if length < 16 + trailer:
                raise _unknown_lifetime(pid, "a COMM record of %d bytes at %d"
                                        % (length, position))
            owner, thread = struct.unpack("<II", read(position + 8, 8))
            if owner == pid and thread == pid and executed is None:
                executed, = struct.unpack("<Q", read(position + length - trailer + time_at, 8))
        elif kind == PERF_RECORD_EXIT:
            if length < 32:
                raise _unknown_lifetime(pid, "an EXIT record of %d bytes at %d"
                                        % (length, position))
            owner, _, thread, _, when = struct.unpack("<IIIIQ", read(position + 8, 24))
            if owner == pid and thread == pid:
                exited = when
        elif kind == PERF_RECORD_AUXTRACE:
            if length < 16:
                raise _unknown_lifetime(pid, "an AUXTRACE record of %d bytes at %d"
                                        % (length, position))
            extra, = struct.unpack("<Q", read(position + 8, 8))
            step += extra
        position += step
    if position != end:
        raise _unknown_lifetime(pid, "the last record ends past its data section")
    if executed is None or exited is None:
        raise _unknown_lifetime(pid, "it holds no %s of it" % (
            "exec" if executed is None else "exit"))
    if exited < executed:
        raise _unknown_lifetime(pid, "it exited before it executed its command")
    return executed, exited


def parse_build_ids(text):
    """[(build ID or None, path)] of `perf buildid-list`; a DSO without one is listed with
    None."""
    ids = []
    for line in text.splitlines():
        if not line.strip():
            continue
        match = _BUILD_ID.match(line.rstrip())
        if match is None:
            raise CaptureError("malformed_build_ids", "perf buildid-list line %r" % line[:200])
        ids.append((match.group(1), match.group(2)))
    return ids


def elf_build_id(data):
    """The GNU build ID note of an ELF image, or None."""
    if data[:4] != b"\x7fELF" or len(data) < 64:
        return None
    is64 = data[4] == 2
    little = data[5] == 1
    order = "little" if little else "big"

    def number(offset, size):
        return int.from_bytes(data[offset:offset + size], order)

    if is64:
        shoff, shentsize, shnum = number(0x28, 8), number(0x3A, 2), number(0x3C, 2)
    else:
        shoff, shentsize, shnum = number(0x20, 4), number(0x2E, 2), number(0x30, 2)
    for index in range(shnum):
        base = shoff + index * shentsize
        if base + shentsize > len(data):
            return None
        kind = number(base + 4, 4)
        if kind != 7:  # SHT_NOTE
            continue
        offset = number(base + (0x18 if is64 else 0x10), 8 if is64 else 4)
        size = number(base + (0x20 if is64 else 0x14), 8 if is64 else 4)
        position = offset
        while position + 12 <= offset + size and position + 12 <= len(data):
            namesz, descsz, note = number(position, 4), number(position + 4, 4), \
                number(position + 8, 4)
            name_start = position + 12
            desc_start = name_start + ((namesz + 3) & ~3)
            if note == 3 and data[name_start:name_start + namesz].rstrip(b"\x00") == b"GNU":
                return data[desc_start:desc_start + descsz].hex()
            position = desc_start + ((descsz + 3) & ~3)
    return None


# Capability --------------------------------------------------------------------------------

# A user-space loop that runs until its stop file exists, so the capture's window never races
# the workload's own end, on a fast host or a slow one.
PROBE_WORKLOAD = ["/bin/sh", "-c", 'while [ ! -e "$1" ]; do i=0; while [ $i -lt 2000 ]; do '
                  'i=$((i+1)); done; done', "capability-loop"]
CAPABILITY_SCRIPT_BYTES = 960 * 1024
# The largest configured tool executable whose bytes are hashed.
EXECUTABLE_BYTES_MAX = 512 * 1024 * 1024


def probe_capability(supervisor, perf, event, frequency, workdir, env, seconds, deadline=None,
                     clock=time.monotonic, binds=()):
    """Whether this host can record `event` under perf's acknowledged control: a real capture
    of a shell loop, enabled for 0.3 s and disabled through the FIFOs and then stopped, whose
    script decodes at least one sample. Every process runs under `supervisor` within `seconds`
    and before `deadline`, a contained one with `binds`. Returns (available, reason, detail,
    version)."""
    deadline = min(deadline, clock() + seconds) if deadline is not None else clock() + seconds
    version = toolrun.run(supervisor, [perf, "--version"], env, seconds, 4096, cwd=workdir,
                          deadline=deadline, clock=clock, binds=binds)
    if version.failure() is not None:
        return False, "perf_unavailable", version.failure(), None
    reported = version.stdout.decode("utf-8", "replace").strip()[:256] or None
    control = PerfControl(workdir)
    data = os.path.join(workdir, "capability.data")
    stop = os.path.join(workdir, "stop")
    argv = record_argv(perf, event, frequency, "frame_pointer", data, control,
                       PROBE_WORKLOAD + [stop])
    running = toolrun.start(supervisor, argv, env, seconds, 4096, cwd=workdir,
                            deadline=deadline, clock=clock, binds=binds)
    try:
        if isinstance(running, toolrun.Result):
            return False, "perf_unavailable", running.failure(), reported
        try:
            control.send(b"enable\n", min(deadline, clock() + 10))
            time.sleep(0.3)
            control.send(b"disable\n", min(deadline, clock() + 10))
        except CaptureError as error:
            return False, error.reason, error.detail, reported
        finally:
            with open(stop, "w"):
                pass
        result = running.wait()
        running = None
        if result.failure() is not None:
            return False, ("perf_timeout" if result.timed_out or result.late
                           else "perf_refused"), "perf record: " + result.failure(), reported
    finally:
        if running is not None and not isinstance(running, toolrun.Result):
            running.cancel()
            running.wait()
        control.close()
    script = toolrun.run(supervisor, [perf, "script", "-i", data, "-F", SCRIPT_FIELDS], env,
                         seconds, CAPABILITY_SCRIPT_BYTES, cwd=workdir, deadline=deadline,
                         clock=clock, binds=binds)
    if script.failure() is not None:
        return False, "perf_script_failed", script.failure(), reported
    try:
        stats = parse_script(script.stdout.decode("utf-8", "replace"))
    except CaptureError as error:
        return False, error.reason, error.detail, reported
    if stats.samples == 0:
        return False, "event_unsupported", ("perf recorded no %s sample while enabled; the "
                                            "event is not available here" % event), reported
    try:
        check_event(stats, event)
    except CaptureError as error:
        return False, error.reason, error.detail, reported
    return True, None, "%d %s samples decoded under acknowledged control" % (stats.samples,
                                                                             event), reported


def fifo_supported():
    return hasattr(os, "mkfifo")


def is_missing(error):
    return isinstance(error, OSError) and error.errno == errno.ENOENT


def executable_identity(path, limit=EXECUTABLE_BYTES_MAX):
    """The SHA-256 of a configured executable, read within `limit` bytes; None beyond it."""
    with open(path, "rb") as stream:
        data = stream.read(limit + 1)
    return sha256_bytes(data) if len(data) <= limit else None
