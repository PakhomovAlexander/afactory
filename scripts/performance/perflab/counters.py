"""Optional diagnostic adapters: Callgrind, DHAT and samply (package p5, ADR-0174, ADR-0175).

Counts explain work performed; they are proxies, never wall-time evidence, and an unchanged
instruction count does not show that a wall-time change is noise.

- **Callgrind** counts instructions of exactly the bracketed region: it starts with its
  instrumentation off (`--instr-atstart=no`), and the region handshake switches it on and off
  with `callgrind_control -i on|off PID`. The `totals:` line of its output is the value.
- **DHAT** counts allocations and allocated bytes of the whole process, setup included, and says
  so (`scope = whole_process`); it has no region switch.
- **samply** is the optional local macOS CPU profile of a self-built executable, saved without a
  viewer (`--save-only`). Its capability check reads the flags its own `record --help` offers and
  refuses targets macOS protects: system paths and hardened-runtime signatures. It never signs,
  installs, changes privileges, opens a browser or uploads. Its capture is the whole process and
  is labelled so.
"""
import json
import os
import re
import zlib

from . import toolrun

_TOTALS = re.compile(r"^totals:\s+([0-9]+)", re.MULTILINE)
_SUMMARY = re.compile(r"^summary:\s+([0-9]+)", re.MULTILINE)
_EVENTS = re.compile(r"^events:\s+(.+)$", re.MULTILINE)
_DHAT_TOTAL = re.compile(r"Total:\s+([0-9,]+) bytes in ([0-9,]+) blocks")
SAMPLY_FLAGS = ("--save-only", "--rate", "--output")
# The most a saved samply profile may inflate to before it is read.
SAMPLY_PROFILE_BYTES_MAX = 256 * 1024 * 1024
PROTECTED_PREFIXES = ("/System/", "/usr/bin/", "/usr/sbin/", "/usr/libexec/", "/bin/", "/sbin/")


class CounterError(Exception):
    def __init__(self, reason, detail):
        Exception.__init__(self, detail)
        self.reason = reason
        self.detail = detail


def callgrind_argv(valgrind, output, command):
    return [valgrind, "--tool=callgrind", "--instr-atstart=no", "--dump-instr=no",
            "--callgrind-out-file=" + output, "--"] + list(command)


def callgrind_switch(supervisor, callgrind_control, env, state, seconds, cwd, target):
    """The region switch: `callgrind_control -i on|off TARGET` under the supervisor, bounded by
    its deadline. `target` is the profiled program's name: the run's supervisor owns its
    process, and the machine lock keeps any other Callgrind run off this machine."""

    def switch(deadline):
        result = toolrun.run(supervisor, [callgrind_control, "-i", state, target], env,
                             seconds, 65536, cwd=cwd, deadline=deadline)
        if result.failure() is not None:
            raise CounterError("callgrind_control_failed", result.failure())

    return switch


def parse_callgrind(text):
    """Instructions (`Ir`) of a Callgrind output file: its one `totals:` line (or, without one,
    its one `summary:` line), whose first value is the first declared event, which must be Ir.
    Both present must agree."""
    events = _EVENTS.search(text)
    if events is None or events.group(1).split()[0] != "Ir":
        raise CounterError("malformed_callgrind", "the Callgrind output declares no Ir event")
    totals, summary = _TOTALS.findall(text), _SUMMARY.findall(text)
    if len(totals) > 1 or len(summary) > 1 or not (totals or summary):
        raise CounterError("malformed_callgrind", "the Callgrind output has %d totals and %d "
                           "summary lines" % (len(totals), len(summary)))
    if totals and summary and totals[0] != summary[0]:
        raise CounterError("malformed_callgrind", "its totals %s and summary %s disagree"
                           % (totals[0], summary[0]))
    return int((totals or summary)[0])


def dhat_argv(valgrind, output, command):
    return [valgrind, "--tool=dhat", "--dhat-out-file=" + output, "--"] + list(command)


def parse_dhat(stderr):
    """(allocations, bytes) from DHAT's `Total: N bytes in M blocks` summary."""
    found = _DHAT_TOTAL.findall(stderr)
    if len(found) != 1:
        raise CounterError("malformed_dhat", "DHAT reported %d totals lines" % len(found))
    allocated, blocks = found[0]
    return int(blocks.replace(",", "")), int(allocated.replace(",", ""))


# samply ----------------------------------------------------------------------------------------

def samply_capability(supervisor, samply, target, env, seconds, deadline=None, cwd="/",
                      codesign="/usr/bin/codesign"):
    """(available, reason, detail, reported version) of a samply capture of `target`; every
    tool runs under `supervisor` before `deadline`."""
    def run(argv, cap):
        return toolrun.run(supervisor, argv, env, seconds, cap, cwd=cwd, deadline=deadline)

    version = run([samply, "--version"], 4096)
    if version.failure() is not None:
        return False, "samply_unavailable", version.failure(), None
    reported = version.stdout.decode("utf-8", "replace").strip()[:256] or None
    helped = run([samply, "record", "--help"], 512 * 1024)
    if helped.failure() is not None:
        return False, "samply_unavailable", helped.failure(), reported
    text = helped.stdout.decode("utf-8", "replace")
    missing = [flag for flag in SAMPLY_FLAGS if flag not in text]
    if missing:
        return False, "samply_flags_missing", ("this samply's record --help offers no %s"
                                               % ", ".join(missing)), reported
    real = os.path.realpath(target)
    if real.startswith(PROTECTED_PREFIXES):
        return False, "protected_executable", ("%s is a system-protected executable macOS does "
                                               "not let a profiler attach to" % real), reported
    if os.path.exists(codesign):
        signed = run([codesign, "-dv", "--verbose=4", real], 65536)
        details = signed.stderr.decode("utf-8", "replace")
        flags = re.search(r"flags=0x[0-9a-f]+\(([^)]*)\)", details)
        if flags is not None and "runtime" in flags.group(1).split(","):
            return False, "hardened_runtime", ("%s is signed with the hardened runtime, which "
                                               "refuses a profiler; the laboratory does not "
                                               "re-sign it" % real), reported
    return True, None, "samply offers %s; %s is self-built and not hardened" % (
        ", ".join(SAMPLY_FLAGS), real), reported


def samply_argv(samply, frequency, output, command):
    return [samply, "record", "--save-only", "--rate", str(frequency), "--output", output,
            "--"] + list(command)


def samply_samples(data):
    """Samples of a saved samply profile (gzip or plain JSON): every thread's sample table,
    or None when the profile's format is not one this reader knows."""
    try:
        if data[:2] == b"\x1f\x8b":
            # Decompressed within a bound, never as far as the stream would go.
            inflated = zlib.decompressobj(16 + zlib.MAX_WBITS)
            data = inflated.decompress(data, SAMPLY_PROFILE_BYTES_MAX + 1)
            if len(data) > SAMPLY_PROFILE_BYTES_MAX or inflated.unconsumed_tail:
                return None
        profile = json.loads(data.decode("utf-8"))
    except (OSError, ValueError, UnicodeDecodeError, zlib.error):
        return None
    threads = profile.get("threads") if isinstance(profile, dict) else None
    if not isinstance(threads, list):
        return None
    total = 0
    for thread in threads:
        samples = thread.get("samples") if isinstance(thread, dict) else None
        if not isinstance(samples, dict):
            return None
        if isinstance(samples.get("length"), int):
            total += samples["length"]
        elif isinstance(samples.get("data"), list):
            total += len(samples["data"])
        else:
            return None
    return total
