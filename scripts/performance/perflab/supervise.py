"""Launch the authority supervisor for one sample and read its framed reply (ADR-0166).

The same hashed `perf-supervisor` bytes measure both sides. It is launched either directly, in
explicitly trusted local mode (`process_group` containment, cases declared without Linux
containment on macOS), or as the native PID 1 of a pinned Linux sample container
(`pid_namespace` containment) built by `container_argv`, in which only the per-sample directory
of the verified scratch filesystem is writable (ADR-0168). Its reply is one
`af.perf-supervisor-result/5` line followed by exactly the retained bytes of the child's stdout
and stderr; a request may add an untimed fixture setup, stdout patterns to count and selectors
of declared fields it observes over the whole stdout as one JSON document (ADR-0172). Timing,
draining, descendant lifecycle and kills are the supervisor's (`review_process::sample`); this
module only frames, bounds and checks the reply and keeps the host-side container preparation
and teardown apart from the child interval.

A host bound, the backstop or the run's total deadline, never kills the supervisor in place of
its sample (ADR-0171): the supervisor is sent SIGTERM, which it forwards to its sample as a
cancellation through the same supervision that bounds the sample, and its reply confirms the
sample's end or says it is uncertain. A supervisor that does not reply within its cancellation
reserve is killed, and nothing its sample could reach is then known to be unused.

A sample container has one owner of its removal, `run_contained`, which removes it once,
however its launch ended, within one allowance that also ends by the run's deadline: a launch
in a container keeps that allowance free before the deadline beside its cancellation reserve,
and a removal not confirmed within it is uncertain (ADR-0173).
"""
import collections
from fractions import Fraction
import os
import signal
import stat
import subprocess
import time

from . import contracts, limits
from .values import ContractError, canonical_bytes, format_decimal, loads_strict, require

HOST_MARGIN_SECONDS = 60
# A cancelled supervisor forwards the signal to its sample, waits a cleanup grace, kills the
# sample, waits another, and verifies its end within a third before it replies (ADR-0171). It is
# given those three graces and this margin to reply.
CANCEL_MARGIN_SECONDS = 10
# The least time a launch takes before the run's deadline beyond its cancellation reserve: the
# supervisor takes over its termination signals first thing, and a signal before that ends it
# with nothing launched.
START_SECONDS = 1
# How long a killed supervisor's pipes are drained before they are closed.
KILL_DRAIN_SECONDS = 5
# At most this long for removing a sample container and confirming it gone, together.
REMOVAL_SECONDS = 60
LAUNCH_ENVIRONMENT = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LC_ALL": "C"}
# The supervisor refuses a request it cannot accept with this exit and no stdout, before it
# launches anything.
REFUSED_EXIT = 2
STDERR_MAX = 64 * 1024
# The only writable path of a sample container: the bind of the per-sample directory of the
# dedicated scratch filesystem. Its parent is the container's read-only root, so nothing the
# child does reaches the filesystem's root or any other entry of it (ADR-0168).
SCRATCH = "/perf/scratch"
SAMPLE_DIRECTORY = "sample"
# The shard's own store of side-written seeds on the same filesystem (ADR-0169): created by the
# shard before its first seed and removed when it ends. A seed's writer request binds only that
# seed's new directory, writable, at SEED_OUTPUT; a sample binds only its seed, read-only, at
# SEED_INPUT.
SEED_DIRECTORY = "seeds"
SEED_OUTPUT = "/perf/seed-out"
SEED_INPUT = "/perf/seed"
# Entries a dedicated scratch filesystem may hold at its root: the filesystem's own protected
# lost+found, never mounted, read or changed, the per-sample directory, and while a shard runs
# its seed store.
SCRATCH_ROOT_ENTRIES = ("lost+found", SAMPLE_DIRECTORY, SEED_DIRECTORY)
# A diagnostic child run's own directory on the verified scratch filesystem: its seeds, the
# private state of each execution and the outputs staged before they are admitted (ADR-0176).
# Only a diagnostic run creates it, and only its contained requests accept it.
DIAGNOSTIC_DIRECTORY = "diagnostic"
# A contained telemetry experiment's own directory on the verified scratch filesystem: its
# corpus and the private state of each execution (ADR-0215). Only a telemetry experiment
# creates it, and only its containers bind anything below it writable.
TELEMETRY_DIRECTORY = "telemetry"
SUPERVISOR_PATH = "/perf/bin/perf-supervisor"
BINARIES = "/perf/bin"
FIXTURES = "/perf/fixtures"

Supervised = collections.namedtuple("Supervised", "result stdout stderr host_wall_ns diagnostics")


class SupervisorFailure(ContractError):
    """The supervisor did not produce a reply its sample can be judged by.

    `launched` says the supervisor was started; `deadline` that the run's total deadline refused
    or cancelled the request; `uncertain` that a process the sample started may still be live,
    so the caller stops and keeps everything that process could reach; `supervised` is the
    reply of a cancelled supervisor, when it gave one."""

    def __init__(self, detail, launched=True, deadline=False, uncertain=False, supervised=None):
        ContractError.__init__(self, detail)
        self.launched = launched
        self.deadline = deadline
        self.uncertain = uncertain
        self.supervised = supervised


def request(argv, environment, cwd, stdin, timeout_ms, stdout_cap, log_cap,
            containment="process_group", scratch=None, grace_ms=5000, setup=None,
            patterns=(), selectors=()):
    """A supervisor request; `setup`, when given, is (argv, environment, cwd, timeout_ms) of
    the untimed fixture step the supervisor runs first, `patterns` the byte patterns it counts
    over the child's whole stdout (ADR-0168) and `selectors` the declared JSON fields it
    observes over it as one document (ADR-0172)."""
    doc = {"schema": contracts.SUPERVISOR_REQUEST_SCHEMA, "argv": list(argv),
           "environment": dict(environment), "cwd": cwd, "stdin": stdin,
           "timeout_ms": timeout_ms, "stdout_cap_bytes": stdout_cap, "log_cap_bytes": log_cap,
           "cleanup_grace_ms": grace_ms, "containment": containment, "scratch": scratch,
           "setup": None, "stdout_patterns": list(patterns),
           "stdout_json": [dict(selector) for selector in selectors]}
    if setup is not None:
        setup_argv, setup_environment, setup_cwd, setup_timeout_ms = setup
        doc["setup"] = {"argv": list(setup_argv), "environment": dict(setup_environment),
                        "cwd": setup_cwd, "timeout_ms": setup_timeout_ms}
    return contracts.validate_supervisor_request(doc)


def read_reply(data, request_doc, where="perf-supervisor"):
    """(result, stdout, stderr) from a framed reply, refused unless every length agrees."""
    end = data.find(b"\n")
    require(0 < end <= limits.RECORD_BYTES_MAX, where + ": no bounded result line")
    result = contracts.validate_supervisor_result(
        loads_strict(data[:end], where, limits.RECORD_BYTES_MAX), where)
    body = data[end + 1:]
    out, err = result["stdout"]["retained"], result["stderr"]["retained"]
    require(len(body) == out + err, where + ": %d payload bytes, the result declares %d"
            % (len(body), out + err))
    require(out <= request_doc["stdout_cap_bytes"]
            and out + err <= request_doc["log_cap_bytes"],
            where + ": retained more than the request's caps")
    require(result["supervisor"]["containment"] == request_doc["containment"],
            where + ": ran another containment than requested")
    require(request_doc["scratch"] is not None or result["scratch"] is None,
            where + ": scratch reported for a request without one")
    require((request_doc["setup"] is None) <= (result["setup"] is None),
            where + ": a setup reported for a request without one")
    require(len(result["stdout_pattern_counts"]) == len(request_doc["stdout_patterns"]),
            where + ": one count per requested stdout pattern")
    observed = result["stdout_json"]
    declared = request_doc["stdout_json"]
    require((observed is None) == (not declared)
            and (observed is None or (len(observed["selectors"]) == len(declared) and all(
                len(item["matches"]) == len(selector["equals"])
                for item, selector in zip(observed["selectors"], declared)))),
            where + ": one JSON observation per requested selector, one count per value")
    return result, body[:out], body[out:]


def backstop_seconds(request_doc, margin=HOST_MARGIN_SECONDS):
    """The host backstop of one request (ADR-0170): the supervisor runs a declared setup and
    then the measured child in sequence, each bounded by its own deadline and cleanup grace, so
    the backstop covers both, plus the host margin. Neither timed region changes."""
    milliseconds = request_doc["timeout_ms"] + request_doc["cleanup_grace_ms"]
    if request_doc["setup"] is not None:
        milliseconds += request_doc["setup"]["timeout_ms"] + request_doc["cleanup_grace_ms"]
    return milliseconds / 1000.0 + margin


def cancel_reserve_seconds(request_doc, margin=None):
    """How long a cancelled supervisor is given to end its sample and reply (ADR-0171): the
    forwarded signal's, the kill's and the verification's cleanup graces, plus the margin
    (`CANCEL_MARGIN_SECONDS` unless given)."""
    margin = CANCEL_MARGIN_SECONDS if margin is None else margin
    return 3 * request_doc["cleanup_grace_ms"] / 1000.0 + margin


def cleanup_confirmed(result):
    """Whether a reply confirms that nothing its sample started is left: the lifecycle of the
    last child it ran (after a failed setup, the setup's) is not uncertain."""
    return result["lifecycle"]["status"] != "cleanup_uncertain"


def launch(launcher, request_doc, deadline=None, clock=time.monotonic,
           margin=HOST_MARGIN_SECONDS, cancel_margin=None, removal_seconds=None, started=None,
           file_bytes=None):
    """Run the supervisor through `launcher` (its argv; the request goes to stdin).

    The host bound only backstops a supervisor that never replies: the request's own deadlines
    and cleanup grace bound the sample. `deadline`, a `clock` instant, is the run's total
    deadline, which dominates (ADR-0171). Nothing is launched unless it leaves the request's
    cancellation reserve and the supervisor's start, and the bound ends that reserve before it.

    When the bound expires the supervisor is sent SIGTERM and forwards it to its sample as a
    cancellation; its reply, within the reserve, says whether the sample's end was confirmed.
    A supervisor that does not reply is killed. Either way the failure says whether anything
    may be live (`uncertain`), never assumes it.

    `removal_seconds` is given for a container: its removal is the caller's (`run_contained`),
    which then decides what is uncertain, and that allowance is kept free before the deadline
    beside the reserve (ADR-0173). Without a container nothing confirms that the sample ended.

    `started`, when given, is called with the supervisor's process once it runs, so a caller
    that interacts with the sample meanwhile can cancel it through the supervisor (SIGTERM),
    never by killing it (ADR-0175). `file_bytes`, when given, is the largest file the
    supervisor and everything it launches may write (`RLIMIT_FSIZE`, inherited): a diagnostic
    output that would outgrow its byte room ends its writer instead (ADR-0175)."""
    data = canonical_bytes(request_doc)
    seconds = backstop_seconds(request_doc, margin)
    reserve = cancel_reserve_seconds(request_doc, cancel_margin)
    after = removal_seconds or 0.0
    bound, by_deadline = "its backstop", False
    if deadline is not None:
        remaining = deadline - clock()
        if remaining < reserve + after + START_SECONDS:
            raise SupervisorFailure(
                "the run's total deadline leaves %.3f s, less than the %.3f s this request's "
                "cancellation%s takes; nothing was launched"
                % (max(remaining, 0.0), reserve + after + START_SECONDS,
                   " and its container's removal" if removal_seconds is not None else ""),
                launched=False, deadline=True)
        if remaining - reserve - after < seconds:
            seconds, bound, by_deadline = (remaining - reserve - after,
                                           "the run's total deadline", True)
    if file_bytes is not None and file_bytes < 1:
        raise SupervisorFailure("no byte room is left for anything this request writes; "
                                "nothing was launched", launched=False)
    launched_ns = time.monotonic_ns()
    process = None
    try:
        try:
            process = subprocess.Popen(launcher, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, env=LAUNCH_ENVIRONMENT,
                                       preexec_fn=(None if file_bytes is None
                                                   else _file_size_limit(file_bytes)))
        except OSError as error:
            raise SupervisorFailure("cannot start the supervisor: %s" % error,
                                    launched=False) from error
        until = None if deadline is None else deadline - after
        reply = _launched_reply(process, data, request_doc, seconds, reserve, bound,
                                by_deadline, launched_ns, started, removal_seconds, until, clock)
    except BaseException as error:
        if process is None and isinstance(error, SupervisorFailure):
            raise  # Popen's proven refusal; no launch ownership was transferred.
        # Popen transferred ownership. A callback or pipe error says nothing about the
        # sample's end. Use the same cancellation protocol, within the original deadline,
        # and accept only a valid supervisor reply as local cleanup evidence.
        supervised = None
        until = None if deadline is None else deadline - after
        try:
            if process is None:
                raise ContractError("Popen did not transfer a process handle")
            out, err, replied = _cancel(process, reserve, until, clock)
            if replied and process.returncode == 0:
                result, child_out, child_err = read_reply(out, request_doc)
                supervised = Supervised(result, child_out, child_err,
                    time.monotonic_ns() - launched_ns, err[-STDERR_MAX:])
        except BaseException as cleanup:
            error.add_note("Supervisor cancellation unconfirmed: %s" % cleanup)
            if not isinstance(cleanup, Exception):
                cleanup.__cause__ = error
                _exceptional_failure(SupervisorFailure(
                    "supervisor cancellation interrupted after launch: %s; %s" % (error, cleanup),
                    uncertain=True), cleanup)
        failure = SupervisorFailure("supervisor launch ownership failed: %s" % error,
                                    uncertain=(supervised is None or
                                               not cleanup_confirmed(supervised.result)),
                                    supervised=supervised)
        _exceptional_failure(failure, error)
    if isinstance(reply, SupervisorFailure):
        raise reply
    return reply


def _exceptional_failure(failure, cause):
    """Keep cancellation/unwind types for synchronous callers, with typed ownership.

    Running transports that same evidence out of its thread; ordinary exceptions become
    SupervisorFailure with their original cause, never an unqualified launch error.
    """
    if not isinstance(cause, Exception):
        cause.supervisor_failure = failure
        raise cause
    raise failure from cause


def _launched_reply(process, data, request_doc, seconds, reserve, bound, by_deadline,
                    launched_ns, started, removal_seconds, until, clock):
    """Read and validate a launched supervisor; launch owns every exceptional exit."""
    cancelled = None
    try:
        if started is not None:
            started(process)
        stdout, stderr = process.communicate(data, timeout=seconds)
    except subprocess.TimeoutExpired:
        cancelled = ("the supervisor did not reply within %.3f s, %s, so its sample was "
                     "cancelled through it" % (seconds, bound))
        stdout, stderr, replied = _cancel(process, reserve, until, clock)
        if not replied:
            cancelled += "; it gave no reply within %.3f s and was killed" % reserve
    host_wall_ns = time.monotonic_ns() - launched_ns
    diagnostics = stderr[-STDERR_MAX:]
    supervised, failure = None, None
    if process.returncode == 0:
        try:
            result, out, err = read_reply(stdout, request_doc)
            supervised = Supervised(result, out, err, host_wall_ns, diagnostics)
        except ContractError as error:
            failure = str(error)
    else:
        failure = "the supervisor exited %d: %s" % (
            process.returncode, diagnostics.decode("utf-8", "replace").strip()[-2048:])
    if failure is None and cancelled is None:
        return supervised
    detail = "; ".join(part for part in (cancelled, failure) if part)
    if supervised is not None:
        uncertain = not cleanup_confirmed(supervised.result)
        detail += "; it replied %s with lifecycle %s: %s" % (
            supervised.result["termination"]["kind"], supervised.result["lifecycle"]["status"],
            supervised.result["lifecycle"]["detail"])
    else:
        # A refusal (its exit 2 without a reply) and death by the signal before the supervisor
        # took it over both come before anything is launched.
        uncertain = not ((process.returncode == REFUSED_EXIT and not stdout)
                         or process.returncode == -signal.SIGTERM)
    if uncertain and removal_seconds is None:
        detail += "; no reply confirms that its sample ended"
    return SupervisorFailure(detail, deadline=by_deadline and cancelled is not None,
                             uncertain=uncertain, supervised=supervised)


def _file_size_limit(file_bytes):
    """The supervisor's pre-exec step: its own and its descendants' file size limit."""
    import resource

    def limit():
        resource.setrlimit(resource.RLIMIT_FSIZE, (file_bytes, file_bytes))

    return limit


def _cancel(process, reserve, deadline=None, clock=time.monotonic):
    """Send the supervisor SIGTERM and wait `reserve` for its reply; kill it when none comes.
    Returns (stdout, stderr, replied)."""
    try:
        process.send_signal(signal.SIGTERM)
    except OSError:
        pass
    def left(seconds):
        return max(0, min(seconds, deadline - clock())) if deadline is not None else seconds

    try:
        stdout, stderr = process.communicate(timeout=left(reserve))
        return stdout, stderr, True
    except subprocess.TimeoutExpired:
        pass
    process.kill()
    try:
        stdout, stderr = process.communicate(timeout=left(KILL_DRAIN_SECONDS))
    except subprocess.TimeoutExpired:
        for stream in (process.stdout, process.stderr):
            stream.close()
        process.wait(timeout=left(KILL_DRAIN_SECONDS))
        stdout, stderr = b"", b""
    return stdout, stderr, False


def outcome(supervised, containment, control_record=True):
    """How a sample's execution ended, before any oracle: (failure kind, detail, stop shard).

    `control_record` says the child's stdout is one bounded control record (the native probe),
    so output past its cap is a mismatch; a CLI command's output may be larger than any
    retained log, and its oracle judges what was retained and counted.

    A failure kind of None means the child ran to its own exit, observed without reaping, with
    a lifecycle this containment accepts; its output is then judged. Pipes alone never
    establish a clean lifecycle, so `no_observed_descendants` is accepted only from the
    explicitly trusted local process-group containment. An exit that was reaped by polling or
    never observed, or whose status is unknown, has no established interval and an uncertain
    cleanup: the shard stops."""
    result = supervised.result
    termination = result["termination"]
    lifecycle = result["lifecycle"]
    if termination["kind"] == "setup_failed":
        if lifecycle["status"] == "cleanup_uncertain":
            return "lifecycle_violation", "the fixture setup's cleanup is uncertain, so the " \
                "shard stops: " + lifecycle["detail"], True
        tail = result["setup"]["stderr_tail"].strip()
        return "launch_failure", "the side's fixture setup failed, so nothing was measured: " \
            + termination["detail"] + ((": " + tail[-1024:]) if tail else ""), False
    if termination["kind"] == "launch_failed":
        return "launch_failure", "the supervisor launched nothing: " + termination["detail"], \
            False
    if lifecycle["status"] == "cleanup_uncertain":
        return "lifecycle_violation", "cleanup is uncertain, so the shard stops: " \
            + lifecycle["detail"], True
    if termination["kind"] == "status_unknown" \
            or result["timing"]["exit_observation"] != "unreaped":
        return "lifecycle_violation", "the child's exit was not observed without reaping " \
            "(%s), so the shard stops" % result["timing"]["exit_observation"], True
    if termination["kind"] == "cancelled":
        return "lifecycle_violation", "the sample was cancelled by signal %d" % \
            termination["signal"], True
    if termination["kind"] == "timeout":
        return "timeout", "the child exceeded its deadline; %s" % lifecycle["detail"], False
    if lifecycle["status"] == "unexpected_descendants":
        return "lifecycle_violation", lifecycle["detail"], False
    if lifecycle["status"] == "no_observed_descendants" and containment != "process_group":
        return "lifecycle_violation", "the containment could not enumerate descendants", True
    if control_record and result["stdout"]["truncated"]:
        return "oracle_mismatch", "the child wrote %d bytes to stdout, more than its %d-byte " \
            "control record" % (result["stdout"]["total"], limits.RECORD_BYTES_MAX), False
    return None, None, False


def optional_metrics(result, wanted):
    """The declared optional metrics a supervised result supports, as exact decimals, and the
    reason for each one it does not (`({key: metric}, {key: reason})`).

    CPU, faults, switches and `wait4_maxrss_bytes` come from the one reaped child's rusage,
    already converted to bytes per platform; each counter is the sum of that record's own
    minor and major (or voluntary and involuntary) fields, never of overlapping records.
    `cgroup_peak_memory_bytes` is the sample container's `memory.peak`, which includes accounted
    cache and the supervisor; it is never presented as RSS."""
    usage = result["rusage"]
    after = result["observations"]["after"]
    metrics, unavailable = {}, {}
    ms = lambda nanos: {"value": format_decimal(Fraction(int(nanos), 1000000)),  # noqa: E731
                        "unit": "ms"}
    sources = {
        "user_cpu_ms": lambda: ms(usage["user_cpu_ns"]),
        "system_cpu_ms": lambda: ms(usage["system_cpu_ns"]),
        "page_faults": lambda: {"value": str(usage["minor_faults"] + usage["major_faults"]),
                                "unit": "count"},
        "context_switches": lambda: {"value": str(usage["voluntary_switches"]
                                                  + usage["involuntary_switches"]),
                                     "unit": "count"},
        "wait4_maxrss_bytes": lambda: {"value": str(usage["maxrss_bytes"]), "unit": "bytes"},
    }
    for key in wanted:
        if key in sources:
            if usage is None:
                unavailable[key] = ("rusage unavailable: %s" % result["rusage_unavailable"])[:256]
            else:
                metrics[key] = sources[key]()
        elif key == "cgroup_peak_memory_bytes":
            entry = after.get("cgroup/memory.peak")
            if entry is not None and "value" in entry and entry["value"].strip().isdigit():
                metrics[key] = {"value": str(int(entry["value"].strip())), "unit": "bytes"}
            else:
                reason = (result["observations"]["unavailable"]
                          or (entry or {}).get("unavailable") or "memory.peak was not observed")
                unavailable[key] = ("cgroup unavailable: %s" % reason)[:256]
    return metrics, unavailable


def returncode(result):
    """The child's status the way the oracle reads it: an exit code, or minus its signal."""
    termination = result["termination"]
    if termination["kind"] == "exited":
        return termination["code"]
    return -termination["signal"]


def summary(supervised):
    """What a diagnostic report shows of one supervised execution."""
    result = supervised.result
    return {"child_wall_ns": result["timing"]["child_wall_ns"],
            "exit_observation": result["timing"]["exit_observation"],
            "host_wall_ns": str(supervised.host_wall_ns),
            "lifecycle": result["lifecycle"]["status"],
            "observation": result["lifecycle"]["observation"],
            "stdout_truncated": result["stdout"]["truncated"],
            "stderr_truncated": result["stderr"]["truncated"]}


def container_argv(docker, name, image, environment, supervisor, binaries, sample_dir, uid,
                   gid, cpus, readonly=None, seed_output=None, binds=None, file_bytes=None):
    """`docker run` for one Linux sample container with the supervisor as its native PID 1.

    Network none, read-only root and binaries, every capability dropped, no new privileges,
    256 PIDs, 4 GiB of memory with memory-swap equal to memory (no swap), a cpuset of the
    declared logical CPUs and no CPU bandwidth quota, the declared non-root user and a private
    cgroup namespace. The only writable path is `sample_dir`, the per-sample directory of the
    verified disk-bounded scratch filesystem, bound at /perf/scratch; its parent is the
    read-only root, so the filesystem's root and its other entries are never exposed and the
    mount root cannot be renamed (ADR-0168). HOME, XDG and TMPDIR of the child live in it.
    `readonly` maps further container paths below /perf to host paths bound read-only (the
    authority's CLI fixtures, a sealed seed). `seed_output`, only for a seed's writer, is that
    seed's new directory in the same filesystem's seed store, bound writable at /perf/seed-out
    (ADR-0169). The container is named, never `--rm`, so its removal is a separate, verified
    step.

    `binds`, only for a diagnostic child run (ADR-0176), lists (host path, writable) pairs bound
    at their own paths: a writable one must lie in the run's own directory of the same verified
    scratch filesystem (`DIAGNOSTIC_DIRECTORY`), so everything the container writes is bounded
    by that filesystem; a read-only one may be any absolute path outside /perf (an executable,
    the authority's fixtures, admitted evidence a tool reads). `file_bytes` is the largest file
    anything in the container may write (`--ulimit fsize`)."""
    require(image.startswith("sha256:") or "@sha256:" in image,
            "the sample image is pinned by digest")
    require(os.path.basename(os.path.normpath(sample_dir)) == SAMPLE_DIRECTORY,
            "the writable bind is the scratch filesystem's per-sample directory, never its root")
    memory = environment["memory_limit_bytes"]
    argv = [docker, "run", "--name", name, "-i", "--network", "none", "--read-only",
            "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
            "--pids-limit", str(environment["pid_limit"]),
            "--memory", str(memory), "--memory-swap", str(memory),
            "--cpuset-cpus", ",".join(str(cpu) for cpu in cpus),
            "--user", "%d:%d" % (uid, gid), "--cgroupns", "private", "--ipc", "none", "--log-driver", "none",
            "--mount", "type=bind,src=%s,dst=%s,readonly" % (supervisor, SUPERVISOR_PATH),
            "--mount", "type=bind,src=%s,dst=%s" % (sample_dir, SCRATCH),
            "--entrypoint", SUPERVISOR_PATH]
    if seed_output is not None:
        store = os.path.join(os.path.dirname(os.path.normpath(sample_dir)), SEED_DIRECTORY)
        require(os.path.dirname(os.path.normpath(seed_output)) == store,
                "a seed writer's only other writable bind is its seed's directory in the "
                "scratch filesystem's seed store")
        argv += ["--mount", "type=bind,src=%s,dst=%s" % (seed_output, SEED_OUTPUT)]
    for name_in_container, path in sorted(binaries.items()):
        argv += ["--mount", "type=bind,src=%s,dst=%s/%s,readonly"
                 % (path, BINARIES, name_in_container)]
    for target, path in sorted((readonly or {}).items()):
        require(target.startswith("/perf/") and target != SCRATCH
                and not target.startswith(SCRATCH + "/") and target != SEED_OUTPUT,
                "read-only fixture binds live below /perf, outside the scratch")
        argv += ["--mount", "type=bind,src=%s,dst=%s,readonly" % (path, target)]
    diagnostic = os.path.join(os.path.dirname(os.path.normpath(sample_dir)),
                              DIAGNOSTIC_DIRECTORY)
    for path, writable in sorted(set(binds or ())):
        require(os.path.isabs(path) and os.path.normpath(path) == path and "," not in path
                and not (path == "/perf" or path.startswith("/perf/")),
                "a diagnostic bind is a plain absolute path outside /perf: %s" % path)
        if writable:
            require(path.startswith(diagnostic + "/"),
                    "a writable diagnostic bind lies in the run's directory of the verified "
                    "scratch filesystem, never elsewhere: %s" % path)
        argv += ["--mount", "type=bind,src=%s,dst=%s%s" % (path, path,
                                                           "" if writable else ",readonly")]
    if file_bytes is not None:
        require(isinstance(file_bytes, int) and file_bytes >= 1,
                "a file size limit of at least one byte")
        argv += ["--ulimit", "fsize=%d:%d" % (file_bytes, file_bytes)]
    require(len(cpus) == environment["logical_cpus"],
            "the sample takes exactly the environment's %d logical CPUs"
            % environment["logical_cpus"])
    require(environment["swap"] == "disabled" and environment["network"] == "disabled"
            and environment["cpu_quota"] == "none",
            "the declared environment is the contained Linux profile")
    return argv + [image]


def _entry(info):
    return {"device": info.st_dev, "inode": info.st_ino, "mode": stat.S_IMODE(info.st_mode),
            "uid": info.st_uid, "directory": stat.S_ISDIR(info.st_mode)}


def prepare_scratch(root, uid, gid, extra=()):
    """Check the verified scratch filesystem at `root` before a sample and return its
    inventory: the identity of every root entry.

    The root may hold only `lost+found`, which is left untouched, `sample` and the running
    shard's seed store `seeds` (ADR-0169). `sample` is
    created when absent, 0700, owned by the container user; when present it must be that
    directory, empty. The container user is either this user or, when the host runs as root,
    handed the directory explicitly: no other host/container user relation is assumed, because
    the host must be able to verify the directory empty afterwards. Any other entry, a symlink,
    a foreign owner or a non-empty `sample` refuses the sample with nothing changed. `extra`
    names further root entries a caller owns: a diagnostic child run's own directory."""
    euid = os.geteuid()
    require(uid == euid or euid == 0,
            "the sample container runs as uid %d, but this host process is uid %d; the "
            "container user must be this user (or the host root) so the host can verify the "
            "per-sample scratch afterwards" % (uid, euid))
    names = sorted(os.listdir(root))
    allowed = SCRATCH_ROOT_ENTRIES + tuple(extra)
    unexpected = [name for name in names if name not in allowed]
    require(not unexpected, "the scratch filesystem %s holds %s beside %s; a sample needs a "
            "fresh filesystem" % (root, ", ".join(unexpected[:8]), ", ".join(allowed)))
    sample = os.path.join(root, SAMPLE_DIRECTORY)
    if SAMPLE_DIRECTORY not in names:
        os.mkdir(sample, 0o700)
        if euid == 0:
            os.chown(sample, uid, gid)
        os.chmod(sample, 0o700)
    info = os.lstat(sample)
    require(stat.S_ISDIR(info.st_mode), "%s is not a directory" % sample)
    require(info.st_uid == uid and stat.S_IMODE(info.st_mode) == 0o700,
            "%s is not the container user's private 0700 directory" % sample)
    require(not os.listdir(sample), "%s is not empty before the sample" % sample)
    inventory = {}
    for name in sorted(os.listdir(root)):
        inventory[name] = _entry(os.lstat(os.path.join(root, name)))
    return inventory


def scratch_problems(root, inventory):
    """Every way the scratch filesystem differs after a sample from its inventory before it:
    a root entry added, removed or replaced (the per-sample directory renamed away and another
    put in its place included), or anything left in the per-sample directory. Empty when the
    child mutated only the per-sample directory and the supervisor cleared it."""
    problems = []
    try:
        names = sorted(os.listdir(root))
    except OSError as error:
        return ["cannot list the scratch filesystem %s: %s" % (root, error)]
    for name in names:
        if name not in inventory:
            problems.append("%s appeared at the scratch filesystem's root" % name)
    for name, before in sorted(inventory.items()):
        try:
            after = _entry(os.lstat(os.path.join(root, name)))
        except OSError as error:
            problems.append("%s: %s" % (name, error.strerror or error))
            continue
        if (after["device"], after["inode"], after["directory"]) != (
                before["device"], before["inode"], before["directory"]):
            problems.append("%s was replaced" % name)
        elif after != before:
            problems.append("%s changed its owner or mode" % name)
    sample = os.path.join(root, SAMPLE_DIRECTORY)
    if not problems:
        try:
            left = sorted(os.listdir(sample))
        except OSError as error:
            left = None
            problems.append("cannot verify %s empty: %s" % (sample, error))
        if left:
            problems.append("%s still holds %s" % (sample, ", ".join(left[:8])))
    return problems


def choose_cpus(online, siblings, count):
    """`count` available logical CPUs, preferring ones on separate physical cores.

    `siblings` maps a CPU to the set of logical CPUs sharing its core; the choice and whether
    it shares a core are both recorded. Affinity never claims exclusive ownership."""
    chosen, cores = [], []
    for cpu in sorted(online):
        core = frozenset(siblings.get(cpu, {cpu}))
        if core not in cores:
            chosen.append(cpu)
            cores.append(core)
        if len(chosen) == count:
            return chosen, False
    for cpu in sorted(online):
        if cpu not in chosen:
            chosen.append(cpu)
        if len(chosen) == count:
            return sorted(chosen), True
    raise ContractError("the host offers %d logical CPUs, fewer than %d"
                        % (len(online), count))


def check_container_observations(result, environment):
    """The resource limits the sample container actually had, from the supervisor's cgroup
    observations; any limit that cannot be verified is listed, never assumed."""
    observed = result["observations"]["before"]
    problems = []

    def value(name):
        entry = observed.get(name)
        return entry["value"].strip() if entry and "value" in entry else None

    expected = (("cgroup/memory.max", str(environment["memory_limit_bytes"])),
                ("cgroup/memory.swap.max", "0"),
                ("cgroup/pids.max", str(environment["pid_limit"])))
    for name, want in expected:
        got = value(name)
        if got != want:
            problems.append("%s is %s, not %s" % (name, got or "unobservable", want))
    quota = value("cgroup/cpu.max")
    if quota is None or not quota.startswith("max "):
        problems.append("cgroup/cpu.max is %s, not an unlimited bandwidth"
                        % (quota or "unobservable"))
    if result["observations"]["unavailable"]:
        problems.append("observations unavailable: " + result["observations"]["unavailable"])
    return problems


Contained = collections.namedtuple("Contained", "supervised removal removed limit_problems "
                                                 "container_overhead_ns scratch_problems")


def run_contained(docker, name, image, environment, supervisor, binaries, scratch_root, uid,
                  gid, cpus, request_doc, readonly=None, seed_output=None, deadline=None,
                  clock=time.monotonic, binds=None, file_bytes=None, started=None, extra=()):
    """One Linux sample in its own container, with the supervisor as PID 1.

    `scratch_root` is the verified dedicated scratch filesystem; only its per-sample directory
    is bound into the container, and its inventory is checked before and after. The container
    is removed afterwards and its removal confirmed; an unconfirmed removal, or a scratch that
    is not exactly as it was with the per-sample directory empty again, is returned for the
    caller to stop the shard. The cgroup limits the supervisor observed are checked against the
    environment: a limit that differs or cannot be observed makes the sample's environment
    unverified, never the side's failure. Container preparation and teardown are host time
    outside the child interval and are returned apart from it. `binds` and `file_bytes` are a
    diagnostic run's (`container_argv`), `started` is called with the `docker run` client once
    it runs, so a cancellation reaches the supervisor through its signal proxy, and `extra`
    names the run's own root entry of the scratch filesystem (ADR-0176)."""
    require(request_doc["containment"] == "pid_namespace"
            and request_doc["scratch"] == SCRATCH,
            "a contained sample uses pid_namespace containment and the mounted scratch")
    try:
        inventory = prepare_scratch(scratch_root, uid, gid, extra)
        sample = os.path.join(scratch_root, SAMPLE_DIRECTORY)
        argv = container_argv(docker, name, image, environment, supervisor, binaries, sample, uid,
                              gid, cpus, readonly, seed_output, binds, file_bytes)
    except (OSError, ContractError) as error:
        raise SupervisorFailure(str(error), launched=False) from error
    # This is the container's one removal, however the launch ends (ADR-0173).
    try:
        supervised = launch(argv, request_doc, deadline=deadline, clock=clock,
                            removal_seconds=REMOVAL_SECONDS, started=started)
    except BaseException as failure:
        evidence = getattr(failure, "supervisor_failure", failure)
        if isinstance(evidence, SupervisorFailure) and not evidence.launched:
            # No `docker run` was started, so there is no container to remove.
            raise
        try:
            removal = remove_container(docker, name, deadline, clock)
        except BaseException as cleanup:
            removal = "container %s removal unconfirmed: %s" % (name, cleanup)
            if not isinstance(cleanup, Exception):
                cleanup.__cause__ = failure
                _exceptional_failure(SupervisorFailure("%s; %s" % (failure, removal),
                    uncertain=True, supervised=getattr(evidence, "supervised", None)), cleanup)
        # A confirmed removal ends every process in the container; anything else may be live.
        _exceptional_failure(SupervisorFailure("%s; %s" % (failure, removal),
                                deadline=getattr(evidence, "deadline", False),
                                uncertain=not removal.endswith(" removed"),
                                supervised=getattr(evidence, "supervised", None)), failure)
    try:
        removal = remove_container(docker, name, deadline, clock)
    except BaseException as cleanup:
        _exceptional_failure(SupervisorFailure(
            "container %s removal unconfirmed: %s" % (name, cleanup),
            uncertain=True, supervised=supervised), cleanup)
    removed = removal.endswith(" removed")
    child = supervised.result["timing"]["child_wall_ns"]
    overhead = supervised.host_wall_ns - int(child) if child is not None else None
    return Contained(supervised, removal, removed,
                     check_container_observations(supervised.result, environment), overhead,
                     scratch_problems(scratch_root, inventory))


def remove_container(docker, name, deadline=None, clock=time.monotonic):
    """Remove a named sample container and confirm it is gone; the outcome as text.

    The removal and its confirmation share one allowance, at most `REMOVAL_SECONDS` and ending
    by `deadline` (a `clock` instant) when given: neither starts once it is spent, and each
    is bounded by what remains of it. A removal not confirmed within it says so."""
    env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LC_ALL": "C"}
    started = clock()
    until = started + REMOVAL_SECONDS
    if deadline is not None:
        until = min(until, deadline)

    def left(step):
        seconds = until - clock()
        if seconds <= 0:
            raise _Spent("no time was left for %s within %.3f s" % (step, until - started))
        return seconds

    try:
        subprocess.run([docker, "rm", "--force", name], stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, timeout=left("its removal"), env=env)
        listed = subprocess.run([docker, "ps", "--all", "--quiet", "--filter",
                                 "name=^/%s$" % name], stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, timeout=left("confirming it"),
                                env=env)
    except (_Spent, OSError, subprocess.TimeoutExpired) as error:
        return "container %s removal unconfirmed: %s" % (name, error)
    if listed.returncode != 0 or listed.stdout.strip():
        return "container %s removal unconfirmed" % name
    return "container %s removed" % name


class _Spent(Exception):
    """A container's removal allowance ended before a step."""


def scratch_environment(root):
    """HOME, XDG and TMPDIR of a child, all inside its private scratch."""
    return {"HOME": root, "TMPDIR": root, "XDG_CACHE_HOME": root, "XDG_CONFIG_HOME": root,
            "XDG_DATA_HOME": root, "XDG_STATE_HOME": root}


def probe_environment(scratch):
    env = {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "TZ": "UTC"}
    env.update(scratch_environment(scratch))
    return env


def absolute(path):
    require(os.path.isabs(path), "%s is not an absolute path" % path)
    return path
