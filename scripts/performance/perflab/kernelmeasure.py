"""The guarded kernel experiment adapter: `cli.py measure --case ID --metric KEY` (p7, ADR-0185;
design section 13).

The kernel's `measure` operator (ADR-0132) runs the staged command once per repetition against
a read-only materialization of the measured Snapshot, owns the repetitions, the whole-command
`elapsed_ms`, the comparison and its requirement for improvement. This command, reached only
through the literal bootstrap of `perflab.kernelguard` after the protected harness verified,
supplies exactly one inner sample per repetition and nothing else: no inner median, paired
comparator or hidden retry.

1. Setup. The source is never written: build output goes to the kernel-provided
   `CARGO_TARGET_DIR` and every log and the probe's scratch to `AF_CHECK_SCRATCH`, both absolute
   and outside the measured tree. The native probe is built with `cargo build --release --locked
   --offline`, so a missing dependency, lock or toolchain input fails instead of being fetched.
   What the target held before the build is observed, never inferred from the policy, and
   recorded on stderr with the build's duration (`kernel-measure setup: {...}`). The target
   remembers the content digest of the whole measured Snapshot it last built from, every
   regular file whatever its directory is called: when it differs, the workspace's own
   fingerprints are removed first, because a materialized Snapshot's files may carry
   modification times Cargo would take for unchanged sources, and a warm target would otherwise
   measure another revision's product. Cargo builds from one bounded copy inside that target,
   checked against the whole Snapshot on every repetition. Identical bytes keep stable paths
   and modification times even when the kernel refreshes its materialization's timestamps.
   Replacing a missing or changed copy also invalidates workspace fingerprints. Dependencies
   stay warm. Setup is outside the inner
   region; the kernel's elapsed time still includes it.
   The build log keeps at most `BUILD_LOG_BYTES` and memory only the tail a failure prints.
2. One sample. One nonspawning native library case of the registry, by instance ID (or case ID
   with one instance), runs once in a fresh scratch below `AF_CHECK_SCRATCH` and is judged by the
   golden oracle; v1 admits only the in-process canonical and CAS cases of `KERNEL_CASES`, never
   a case that spawns (source capture runs Git), needs containment, reads a seed or repeats.
   Its stdout and stderr together are bounded by the registry's `sample_log_bytes` while they
   are read: a probe that outgrows it or its timeout is killed at once and fails, and one whose
   descendant still holds its streams after it exited fails too. The probe stays in the
   measure's process group, which the kernel's check runner ends when the measure exits.
3. The report. Only on a passed oracle, the last stdout line is the existing
   `af.measure-report/1` with exactly the requested metric, a metric the case declares, in its
   declared unit and canonical decimal. Every failure exits nonzero with a diagnostic on stderr
   and prints no report.
"""
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import threading
import time

from . import contracts, limits, oracle, supervise
from .values import ContractError, check_decimal, require

REPORT_SCHEMA = "af.measure-report/1"
REPORT_LINE_MAX = 64 * 1024
SUITE = "fixtures/performance/suite.toml"
# The only cases v1 measures under the kernel: in-process serialization and CAS reads, which
# spawn nothing, need no containment and read no seed.
KERNEL_CASES = ("canonical_serialize_small", "canonical_serialize", "canonical_validate_small",
                "canonical_validate", "cas_raw_get_small", "cas_raw_get", "cas_typed_get_small",
                "cas_typed_get")
# The kernel's metric units (af.code-task-policy/1); the registry's are the same names.
KERNEL_UNITS = ("ms", "bytes", "count", "ratio")
PROBE_PACKAGE = "review-pipeline"
PROBE_EXAMPLE = "perf-probe"
BUILD_TIMEOUT_SECONDS = 45 * 60
# The build log kept in the scratch, at most; the build's output beyond it is counted, not kept.
BUILD_LOG_BYTES = limits.BUILD_LOG_BYTES_DEFAULT
# The tail of a failed build or probe's diagnostics kept in memory and printed, at most.
BUILD_LOG_TAIL = 4096
# How long a stream may stay open after its process exited before a descendant is taken to hold
# it (`supervise.KILL_DRAIN_SECONDS`).
DRAIN_SECONDS = supervise.KILL_DRAIN_SECONDS
SETUP_PREFIX = "kernel-measure setup: "
# Where the bound target records the workspace sources it last built (`build_probe`).
SOURCES_RECORD = "af-kernel-measure-sources.json"
SOURCES_COPY = "af-kernel-measure-source"
SOURCES_SCHEMA = "af.perf-kernel-sources/3"
SOURCES_ENTRIES_MAX = 200000
SOURCES_BYTES_MAX = 2 * limits.GIB


class Failed(Exception):
    """The repetition failed: exit nonzero with this diagnostic and no report."""


class Captured(object):
    """What one bounded run left: its exit, the first `head` bytes of stdout, the last `tail`
    bytes of stderr (of the merged stream when stderr went to stdout), how many bytes its
    streams carried and how many of them a log kept, and why it was stopped, if it was."""

    def __init__(self):
        self.code = None
        self.head = b""
        self.tail = b""
        self.total = 0
        self.logged = 0
        self.over = False
        self.timed_out = False
        self.held = False

    def stopped(self):
        """Why the run is unusable whatever its exit code, or None."""
        if self.over:
            return "wrote more than its bound on stdout and stderr (%d bytes counted) and was " \
                   "killed" % self.total
        if self.timed_out:
            return "did not finish within its bound and was killed"
        if self.held:
            return "exited, but a process it started still held its streams %d s later" \
                   % DRAIN_SECONDS
        return None


def capture(argv, cwd, env, timeout_seconds, cap, stdin=None, merged=False, head=0, tail=0,
            log=None, log_bytes=0, clock=time.monotonic):
    """Run `argv` once, bounding what it can make this process hold while it runs: stdout and
    stderr together at most `cap` bytes (None: unbounded, counted), of which only `head` bytes of
    stdout, a `tail` of stderr and `log_bytes` of a `log` file are kept. A run that outgrows
    `cap` or `timeout_seconds` is killed at once. The child stays in this process's group, the
    one the kernel's check runner ends when the measure exits (ADR-0132), so nothing it starts
    escapes that cleanup; a stream still held after the child's exit is a failure here."""
    result = Captured()
    lock = threading.Lock()
    try:
        child = subprocess.Popen(argv, cwd=cwd, env=env,
                                 stdin=subprocess.DEVNULL if stdin is None else subprocess.PIPE,
                                 stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT if merged else subprocess.PIPE)
    except OSError as error:
        raise Failed("%s could not start: %s" % (argv[0], error))
    killed = []

    def kill():
        # Exact while the child is unreaped: its pid cannot have been reused.
        if not killed:
            killed.append(True)
            try:
                child.kill()
            except OSError:
                pass

    def keep(chunk, from_stdout):
        with lock:
            result.total += len(chunk)
            if from_stdout and len(result.head) < head:
                result.head += chunk[:head - len(result.head)]
            if (merged or not from_stdout) and tail:
                result.tail = (result.tail + chunk)[-tail:]
            if log is not None and result.logged < log_bytes:
                part = chunk[:log_bytes - result.logged]
                try:
                    log.write(part)
                    result.logged += len(part)
                except (OSError, ValueError):
                    # The step already ended with its stream held, and its log was closed.
                    pass
            if cap is not None and result.total > cap and not result.over:
                result.over = True
                kill()

    def drain(pipe, from_stdout):
        descriptor = pipe.fileno()
        while True:
            try:
                chunk = os.read(descriptor, 64 * 1024)
            except OSError:
                break
            if not chunk:
                break
            keep(chunk, from_stdout)

    def feed():
        try:
            child.stdin.write(stdin)
        except OSError:
            pass
        finally:
            try:
                child.stdin.close()
            except OSError:
                pass

    threads = [threading.Thread(target=drain, args=(child.stdout, True))]
    if not merged:
        threads.append(threading.Thread(target=drain, args=(child.stderr, False)))
    if stdin is not None:
        threads.append(threading.Thread(target=feed))
    for thread in threads:
        thread.daemon = True
        thread.start()
    deadline = clock() + timeout_seconds
    while child.poll() is None:
        left = deadline - clock()
        if left <= 0:
            with lock:
                result.timed_out = not result.over
                kill()
            break
        try:
            child.wait(timeout=min(left, 0.05))
        except subprocess.TimeoutExpired:
            pass
    child.wait()
    result.code = child.returncode
    drained = clock() + DRAIN_SECONDS
    for thread in threads:
        thread.join(max(0.0, drained - clock()))
        if thread.is_alive():
            result.held = True
    for pipe in (child.stdout, child.stderr):
        if pipe is not None and not result.held:
            pipe.close()
    return result


def resolve_instance(doc, ident):
    """The expanded instance `ident` names: an instance ID, or a case ID with one instance."""
    instances = {}
    for item, _, repetitions in oracle.reference.native_instances(doc):
        instances[item["instance_id"]] = (item, repetitions)
    if ident in instances:
        item, repetitions = instances[ident]
    else:
        named = [value for value in instances.values() if value[0]["case_id"] == ident]
        if len(named) != 1:
            raise Failed("--case %s names %s; give one native instance ID" % (
                ident, "no native case or instance" if not named
                else "%d instances (%s)" % (len(named), ", ".join(
                    sorted(value[0]["instance_id"] for value in named)))))
        item, repetitions = named[0]
    case_id = item["case_id"]
    case = doc["cases"].get(case_id)
    if case_id not in KERNEL_CASES or case is None:
        raise Failed("%s is not a nonspawning native library case of the kernel adapter; v1 "
                     "measures only %s" % (item["instance_id"], ", ".join(KERNEL_CASES)))
    require(case["kind"] == "library" and not case["requires_linux_containment"]
            and case["oracle"]["state"] == "available"
            and case["execution"]["state"] == "supported"
            and oracle.suite_module.seed_of(doc, item) is None and repetitions == 1,
            "%s is not a supported one-sample nonspawning library case" % item["instance_id"])
    return item, case


def metric_unit(doc, item, case, metric):
    """The declared unit of `metric`, which the case must declare."""
    declared = [case["primary_metric"]] + list(case["optional_metrics"])
    if metric not in declared:
        raise Failed("--metric %s is not a metric case %s declares (%s)"
                     % (metric, item["case_id"], ", ".join(declared)))
    unit = doc["metrics"][metric]["unit"]
    require(unit in KERNEL_UNITS, "metric %s is in %s, which no kernel measure declares"
            % (metric, unit))
    return unit


def report_line(metric, value, unit):
    """The one af.measure-report/1 line, after checking its value is canonical."""
    check_decimal(value, "metric " + metric)
    require(unit in KERNEL_UNITS, "unit %s is not a kernel unit" % unit)
    line = json.dumps({"schema": REPORT_SCHEMA,
                       "metrics": {metric: {"value": value, "unit": unit}}},
                      sort_keys=True, separators=(",", ":"))
    require(len(line.encode("utf-8")) < REPORT_LINE_MAX, "the report exceeds 64 KiB")
    return line


def _outside(root, path, name):
    require(path and os.path.isabs(path), "%s is not set to an absolute path; the kernel binds "
            "it for every measure repetition" % name)
    real, tree = os.path.realpath(path), os.path.realpath(root)
    require(real != tree and not real.startswith(tree + os.sep),
            "%s %s is inside the measured Snapshot, which is never written" % (name, path))
    return real


def _sha256(path):
    try:
        digest = hashlib.sha256()
        with open(path, "rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        return "sha256:" + digest.hexdigest()
    except OSError:
        return None


def _target_state(target):
    """What the bound target held before the build: nothing, or something."""
    try:
        return "populated" if os.listdir(target) else "empty"
    except FileNotFoundError:
        return "empty"


def workspace_members(root):
    """{package name: member directory} of the measured workspace's root manifest."""
    from . import tomlfile
    with open(os.path.join(root, "Cargo.toml"), "rb") as stream:
        manifest = tomlfile.loads(stream.read(), "Cargo.toml")
    members = {}
    for member in manifest.get("workspace", {}).get("members", []):
        require(isinstance(member, str) and "*" not in member and "?" not in member
                and not os.path.isabs(member) and ".." not in member.split("/"),
                "workspace member %r is not an explicit relative path" % (member,))
        with open(os.path.join(root, member, "Cargo.toml"), "rb") as stream:
            package = tomlfile.loads(stream.read(), member + "/Cargo.toml").get("package", {})
        require(isinstance(package.get("name"), str), "%s has no package name" % member)
        members[package["name"]] = member
    return members


def sources_digest(root):
    """The content digest of every regular file of the measured Snapshot: the closure Cargo may
    compile from. A workspace package reads more than its member directory (schemas, fixtures
    and brand tokens through `include_str!`, the root `.cargo` configuration), and a source
    directory may have any name, `target` included, so nothing is skipped by name. Content, never
    a modification time, decides; the guard has already refused every link and special file,
    and anything else is refused here too."""
    digest = hashlib.sha256(SOURCES_SCHEMA.encode("utf-8") + b"\n")
    paths, pending, seen = [], [""], 0
    while pending:
        relative = pending.pop()
        with os.scandir(os.path.join(root, relative) if relative else root) as listing:
            for entry in listing:
                seen += 1
                require(seen <= SOURCES_ENTRIES_MAX, "the measured Snapshot holds more than %d "
                        "entries" % SOURCES_ENTRIES_MAX)
                path = relative + "/" + entry.name if relative else entry.name
                if entry.is_dir(follow_symlinks=False):
                    pending.append(path)
                elif entry.is_file(follow_symlinks=False):
                    paths.append(path)
                else:
                    raise ContractError("%s is not a regular file or directory" % path)
    total = 0
    for path in sorted(paths, key=lambda item: item.encode("utf-8", "surrogateescape")):
        descriptor = os.open(os.path.join(root, path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        content, size = hashlib.sha256(), 0
        with os.fdopen(descriptor, "rb") as stream:
            executable = bool(os.fstat(stream.fileno()).st_mode & 0o111)
            while True:
                chunk = stream.read(1024 * 1024)
                if not chunk:
                    break
                size += len(chunk)
                total += len(chunk)
                require(total <= SOURCES_BYTES_MAX, "the measured Snapshot holds more than %d "
                        "bytes" % SOURCES_BYTES_MAX)
                content.update(chunk)
        digest.update(("%s\0%d\0%d\0%s\n" % (path, size, executable, content.hexdigest())).encode(
            "utf-8", "surrogateescape"))
    return "sha256:" + digest.hexdigest()


def invalidate_workspace(target, members):
    """Remove the release fingerprints of the workspace's own packages, so Cargo recompiles them
    from the measured sources; dependency builds stay warm. A materialized Snapshot's files may
    carry modification times older than the bound target's outputs, which Cargo's freshness
    check would take for an unchanged source and reuse another revision's product."""
    root = os.path.join(target, "release", ".fingerprint")
    removed = 0
    try:
        entries = sorted(os.listdir(root))
    except FileNotFoundError:
        return 0
    for name in entries:
        package, _, suffix = name.rpartition("-")
        if package in members and len(suffix) == 16 \
                and all(char in "0123456789abcdef" for char in suffix):
            shutil.rmtree(os.path.join(root, name))
            removed += 1
    return removed


def _recorded_sources(path):
    try:
        with open(path, "rb") as stream:
            doc = json.loads(stream.read(4096).decode("utf-8"))
        return doc.get("sources") if isinstance(doc, dict) else None
    except (OSError, ValueError):
        return None


def stable_sources(root, target, sources):
    """One content-checked source copy under the kernel's exclusively leased, bounded target.

    A native repetition receives a new materialization with refreshed mtimes (ADR-0161).
    Building there cannot stay warm. Preserve this copy only when its actual bytes still
    match the entire measured Snapshot; a marker alone never authorizes reuse. Always run
    Cargo, and invalidate workspace fingerprints whenever this copy must be replaced.
    No per-Snapshot directories accumulate. A failed copy/build cannot attest freshness.
    """
    os.makedirs(target, exist_ok=True)
    path = os.path.join(target, SOURCES_COPY)
    if os.path.lexists(path):
        require(not os.path.islink(path) and os.path.isdir(path),
                "the kernel measure source copy is not a regular directory")
        # This also refuses links and special files before removal or use.
        if sources_digest(path) == sources:
            return path, False
        # Recover a partial copy interrupted before its directory modes were normalized.
        for directory, _, _ in os.walk(path):
            os.chmod(directory, os.stat(directory).st_mode | 0o700)
        shutil.rmtree(path)
    # root has already passed the entry/byte bounds and is the kernel's immutable Snapshot.
    # copy, unlike copy2, gives changed sources fresh mtimes. Preserve links if one appears
    # unexpectedly so the verification below refuses it instead of following it.
    shutil.copytree(root, path, symlinks=True, copy_function=shutil.copy)
    require(sources_digest(path) == sources,
            "the kernel measure source copy differs from the measured Snapshot")
    # Snapshot directories may be read-only. This owned cache copy must be removable on the
    # next revision; source bytes and file modes stay unchanged.
    for directory, _, _ in os.walk(path):
        os.chmod(directory, os.stat(directory).st_mode | 0o700)
    return path, True


def build_probe(root, environ, scratch, clock=time.monotonic):
    """Build the probe offline into the kernel-provided target; (binary path, setup record).
    The target remembers the digest of the workspace sources it last built; other sources
    invalidate the workspace's own fingerprints first, so a warm target never measures another
    revision's product."""
    target = _outside(root, environ.get("CARGO_TARGET_DIR"), "CARGO_TARGET_DIR")
    binary = os.path.join(target, "release", "examples", PROBE_EXAMPLE)
    state = _target_state(target)
    before = _sha256(binary)
    members = workspace_members(root)
    sources = sources_digest(root)
    record_path = os.path.join(target, SOURCES_RECORD)
    previous = _recorded_sources(record_path)
    build_root, replaced = stable_sources(root, target, sources)
    invalidated = invalidate_workspace(target, members) if previous != sources or replaced else 0
    env = dict(environ, CARGO_NET_OFFLINE="true", CARGO_TERM_COLOR="never")
    for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS",
                 "RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"):
        env.pop(name, None)
    argv = ["cargo", "build", "--release", "--locked", "--offline", "-p", PROBE_PACKAGE,
            "--example", PROBE_EXAMPLE]
    started = clock()
    # The log keeps at most BUILD_LOG_BYTES and memory only the tail a failure prints; the
    # build's whole output is counted, never held.
    with open(os.path.join(scratch, "build.log"), "wb") as log:
        built = capture(argv, build_root, env, BUILD_TIMEOUT_SECONDS, None, merged=True,
                        tail=BUILD_LOG_TAIL, log=log, log_bytes=BUILD_LOG_BYTES, clock=clock)
    build_ms = int((clock() - started) * 1000)
    stopped = built.stopped()
    if stopped is not None or built.code != 0:
        raise Failed("the offline locked probe build %s after %d ms; a missing dependency, lock "
                     "or toolchain input is never fetched:\n%s" % (
                         stopped or "exited %d" % built.code, build_ms,
                         built.tail.decode("utf-8", "replace")))
    after = _sha256(binary)
    require(after is not None, "the build left no %s" % binary)
    with open(record_path, "w") as stream:
        json.dump({"sources": sources}, stream)
    record = {"target": "kernel_bound", "state_before": state, "probe_before": before,
              "sources": sources, "sources_before": previous,
              "workspace_fingerprints_invalidated": invalidated, "probe": after,
              "probe_rebuilt": before != after, "build_ms": build_ms,
              "build_log": {"bytes": built.total, "kept": built.logged},
              "cargo_home": "bound" if environ.get("CARGO_HOME") else "unset",
              "scratch": "af_check_scratch"}
    return binary, record


def sample(doc, golden, binary, item, scratch, timeout_seconds):
    """Run the probe once in a fresh scratch and judge it; the oracle's check record."""
    directory = tempfile.mkdtemp(prefix="probe-", dir=scratch)
    request = oracle.request_for(doc, item["instance_id"], directory)
    env = supervise.probe_environment(directory)
    # The registry's sample log bound holds stdout and stderr together while they are read; the
    # oracle sees at most one record more than a probe message may hold, so an overlong stdout
    # is judged as overlong, never as a parsed prefix.
    completed = capture([binary], directory, env, timeout_seconds,
                        doc["limits"]["sample_log_bytes"],
                        stdin=contracts.canonical_bytes(request),
                        head=limits.RECORD_BYTES_MAX + 1, tail=BUILD_LOG_TAIL)
    stopped = completed.stopped()
    if completed.timed_out:
        raise Failed("the probe exceeded the case's %d-second sample timeout" % timeout_seconds)
    if stopped is not None:
        raise Failed("the probe %s%s" % (stopped, (
            "\nprobe stderr: " + completed.tail.decode("utf-8", "replace"))
            if completed.tail else ""))
    result = oracle.judge(doc, golden, request, completed.code, completed.head)
    return result, completed.tail


def measure(root, ident, metric, environ, out, err):
    """One repetition: setup, one judged sample, one report line. Returns the exit code."""
    try:
        scratch = _outside(root, environ.get("AF_CHECK_SCRATCH"), "AF_CHECK_SCRATCH")
        require(os.path.isdir(scratch), "AF_CHECK_SCRATCH %s is not a directory" % scratch)
        doc, golden = oracle.load(os.path.join(root, SUITE))
        item, case = resolve_instance(doc, ident)
        unit = metric_unit(doc, item, case, metric)
        work = tempfile.mkdtemp(prefix="kernel-measure-", dir=scratch)
        binary, setup = build_probe(root, environ, work)
        setup.update(instance_id=item["instance_id"], metric=metric)
        err.write(SETUP_PREFIX + json.dumps(setup, sort_keys=True) + "\n")
        result, probe_stderr = sample(doc, golden, binary, item, work,
                                      oracle.timeout_for(doc, item["instance_id"]))
        if result["status"] != "passed":
            raise Failed("%s %s: %s: %s%s" % (
                item["instance_id"], result["status"], result["failure"]["kind"],
                result["failure"]["detail"],
                ("\nprobe stderr: " + probe_stderr.decode("utf-8", "replace"))
                if probe_stderr else ""))
        value = result["metrics"].get(metric)
        if value is None or value["unit"] != unit:
            raise Failed("the judged sample of %s reported no %s in %s"
                         % (item["instance_id"], metric, unit))
        line = report_line(metric, value["value"], unit)
    except Failed as error:
        err.write("kernel-measure: failed, no report: %s\n" % error)
        return 1
    except (ContractError, OSError) as error:
        err.write("kernel-measure: refused, no report: %s\n" % error)
        return 2
    err.write("kernel-measure: %s %s = %s %s, oracle passed\n"
              % (item["instance_id"], metric, value["value"], unit))
    out.write(line + "\n")
    out.flush()
    return 0
