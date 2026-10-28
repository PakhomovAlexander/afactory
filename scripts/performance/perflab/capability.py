"""What this host can execute, with actual identities and the reason for every gap (ADR-0166).

Nothing here installs, configures or grants anything, and nothing missing is replaced by a
weaker mode. Trusted local execution is the explicit macOS capability for discovery and
nonspawning library cases. Linux samples need a Linux container runtime with cgroup v2 and a
configured, verified disk-bounded scratch filesystem of at most the environment's
2-GiB scratch limit: tmpfs, a container's root layer and an unbounded bind mount are not one,
and a host disk image's or `statvfs`'s report alone does not establish the bound, so
enforcement is verified by filling it. Only the filesystem's own capacity refusal (ENOSPC or
EDQUOT) after real progress, with the data it accepted synced, the filesystem then reporting
itself full and the fill file removed, verifies it (ADR-0167); an I/O or permission error, no
progress, the fill's time bound and a failed cleanup each refuse it with the original error. An
environment is available only where this host provides its containment and the registry
declares it executable (ADR-0168); otherwise it names every reason, the registry's own included.

The whole check, its metadata steps and the fill's every write, sync and cleanup, is bounded by
one absolute deadline fixed before its first step (ADR-0173): no step starts once it has passed,
no write starts in the cleanup allowance before it, an answer that arrives after its bound is
the deadline's, never a capacity refusal, and a fill still blocked at the deadline is abandoned
with its file kept and its cleanup unconfirmed.
"""
import errno
import os
import platform
import subprocess
import threading
import time

from . import contracts

BLOCK_FILESYSTEMS = ("ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs")
FILL_CHUNK = 1 << 20
# The fill syncs its file each time this many bytes were written since its last sync, and once
# more after the filesystem refuses a write. A sync after every chunk is one durable flush per
# MiB: the 120 s check of an 8-GiB scratch on a nested loop ext4 stopped at its fill's time bound
# with 1.46 GB written. The bound on the unsynced data keeps each sync short.
FILL_SYNC_BYTES = 64 * FILL_CHUNK
# At most this long for one scratch check, its fill included: 2 GiB written and synced well within
# it on a disk that a sample's scratch can live on; a slower one is not verified.
FILL_SECONDS_MAX = 600
# Kept free before the deadline for what follows the fill's last write: the free space at its
# stop, closing and removing its file. No write starts within it.
FILL_CLEANUP_SECONDS = 30
# The start of a refusal reason of a step the deadline left no time for.
DEADLINE_REASON = "the check's deadline passed before "
# Errors that are the filesystem refusing more data: its capacity or its quota.
CAPACITY_ERRNOS = (errno.ENOSPC, errno.EDQUOT)
TOOL_TIMEOUT_SECONDS = 30


class Host(object):
    """The filesystem calls a scratch check makes, its clock and how it waits for its fill;
    tests replace one with a deterministic failure or delay. `clock` is the clock the check's
    deadline is an instant of."""
    open = staticmethod(os.open)
    write = staticmethod(os.write)
    fsync = staticmethod(os.fsync)
    close = staticmethod(os.close)
    unlink = staticmethod(os.unlink)
    statvfs = staticmethod(os.statvfs)
    listdir = staticmethod(os.listdir)
    realpath = staticmethod(os.path.realpath)
    clock = staticmethod(time.monotonic)

    def __init__(self, clock=None):
        if clock is not None:
            self.clock = clock

    def mount_of(self, path, mountinfo):
        return mount_of(path, mountinfo)

    def wait(self, event, seconds):
        """Whether `event` was set within `seconds` of real time."""
        return event.wait(max(0.0, seconds))


def host_platform():
    system = platform.system()
    return {"Darwin": "macos", "Linux": "linux"}.get(system, system.lower() or "unknown")


def _tool(argv, first_line=True, timeout=TOOL_TIMEOUT_SECONDS):
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LC_ALL": "C"}
    try:
        result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env,
                                timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"identity": None, "unavailable": ("%s: %s" % (argv[0], error))[:1024]}
    text = (result.stdout or result.stderr).decode("utf-8", "replace").strip()
    if result.returncode != 0 or not text:
        detail = result.stderr.decode("utf-8", "replace").strip().splitlines()
        return {"identity": None, "unavailable": ("%s exited %d%s" % (
            " ".join(argv), result.returncode, (": " + detail[-1]) if detail else ""))[:1024]}
    return {"identity": (text.splitlines()[0] if first_line else text)[:512],
            "unavailable": None}


def mount_of(path, mountinfo="/proc/self/mountinfo"):
    """(mount point, filesystem type) of the mount holding the resolved `path` on Linux."""
    real = path
    best = None
    try:
        with open(mountinfo, "rb") as stream:
            lines = stream.read().decode("utf-8", "replace").splitlines()
    except OSError:
        return None
    for line in lines:
        fields = line.split(" ")
        if " - " not in line or len(fields) < 5:
            continue
        point = fields[4].replace("\\040", " ")
        fstype = line.split(" - ", 1)[1].split(" ")[0]
        if real == point or real.startswith(point.rstrip("/") + "/"):
            if best is None or len(point) > len(best[0]):
                best = (point, fstype)
    return best


def scratch_capability(path, limit_bytes, verify_enforcement=False, system=None,
                       mountinfo="/proc/self/mountinfo", host=None, deadline=None):
    """The bounded-scratch backend at `path`: a dedicated Linux block filesystem mounted there,
    no larger than `limit_bytes`, empty but for `lost+found` and a `sample` directory, whose
    enforcement is verified by writing until the filesystem refuses. Only `sample` is ever bound
    into a sample container; the root and `lost+found` stay outside it (ADR-0168).

    `deadline`, an instant of `host.clock`, bounds the whole check (ADR-0173): by default
    `FILL_SECONDS_MAX` after it starts. Each step, resolving the path, finding its mount, its
    `statvfs`, its listing and the fill, starts only before it, and a step it left no time for
    refuses the check with a reason starting `DEADLINE_REASON`."""
    system = system or host_platform()
    host = host or Host()
    if deadline is None:
        deadline = host.clock() + FILL_SECONDS_MAX
    record = {"backend": "none", "path": None, "filesystem": None, "capacity_bytes": None,
              "enforcement": "not_verified", "fill": None,
              "capability": {"status": "unavailable", "reasons": []}}
    reasons = record["capability"]["reasons"]
    if path is None:
        reasons.append("no dedicated disk-bounded scratch filesystem is configured; tmpfs, the "
                       "container root layer and an unbounded bind mount do not satisfy the "
                       "per-sample scratch contract")
        return record

    def late(step):
        if host.clock() < deadline:
            return False
        reasons.append(DEADLINE_REASON + step + "; it was not started")
        record["enforcement"] = "refused"
        return True

    if late("resolving its path"):
        return record
    record.update(backend="dedicated_filesystem", path=host.realpath(path))
    if system != "linux":
        reasons.append("the scratch filesystem must be a mount of the Linux host that runs "
                       "the containers; on %s a container runtime's file sharing cannot be "
                       "verified as one" % system)
        record["enforcement"] = "refused"
        return record
    if late("finding its mount"):
        return record
    mounted = host.mount_of(record["path"], mountinfo)
    if mounted is None or mounted[0] != record["path"]:
        reasons.append("%s is not itself a mount point" % record["path"])
        record["enforcement"] = "refused"
        return record
    record["filesystem"] = mounted[1]
    if mounted[1] not in BLOCK_FILESYSTEMS:
        reasons.append("%s is %s, not a disk-backed block filesystem" % (
            record["path"], mounted[1]))
        record["enforcement"] = "refused"
        return record
    if late("reading its capacity"):
        return record
    try:
        info = host.statvfs(record["path"])
    except OSError as error:
        reasons.append("statvfs failed: %s" % _error(error))
        record["enforcement"] = "refused"
        return record
    record["capacity_bytes"] = info.f_blocks * info.f_frsize
    if record["capacity_bytes"] > limit_bytes:
        reasons.append("its reported capacity %d bytes exceeds the %d-byte scratch limit"
                       % (record["capacity_bytes"], limit_bytes))
        record["enforcement"] = "refused"
        return record
    if late("listing it"):
        return record
    try:
        entries = host.listdir(record["path"])
    except OSError as error:
        reasons.append("listing it failed: %s" % _error(error))
        record["enforcement"] = "refused"
        return record
    extra = sorted(set(entries) - {"lost+found", "sample"})
    if extra:
        reasons.append("it is not empty: " + ", ".join(extra[:8]))
        record["enforcement"] = "refused"
        return record
    if not verify_enforcement:
        reasons.append("enforcement not verified; run `cli.py capabilities --scratch PATH "
                       "--verify-scratch-enforcement` on this host")
        return record
    if host.clock() >= deadline - FILL_CLEANUP_SECONDS:
        # No fill file is created: none could be written and removed within the deadline.
        reasons.append(DEADLINE_REASON + "filling it, beyond the %d s kept for its cleanup; "
                       "it was not started" % FILL_CLEANUP_SECONDS)
        record["enforcement"] = "refused"
        return record
    record["fill"] = fill(record["path"], limit_bytes, host, deadline)
    why = fill_refusal(record["fill"], limit_bytes)
    if why is not None:
        reasons.append(why)
        record["enforcement"] = "refused"
        return record
    record["enforcement"] = "verified"
    record["capability"]["status"] = "available"
    return record


def ended_by_deadline(record):
    """Whether a scratch check was refused by its deadline: a step it left no time for, or a
    fill it stopped."""
    return ((record["fill"] or {}).get("stopped") == "deadline"
            or any(reason.startswith(DEADLINE_REASON)
                   for reason in record["capability"]["reasons"]))


def _error(error):
    """An OSError as its errno name and text, the way a refusal keeps it."""
    name = errno.errorcode.get(error.errno, "errno %s" % error.errno)
    return ("%s: %s" % (name, error.strerror or error))[:512]


class _Progress(object):
    """What a fill's worker shares with the caller waiting for it."""

    def __init__(self):
        self.lock = threading.Lock()
        self.done = threading.Event()
        self.abandoned = False
        self.stage = "open"
        self.written = 0
        self.record = None


def fill(root, limit_bytes, host=None, deadline=None):
    """Write zeros into one new file below `root`, a MiB at a time, until the filesystem
    refuses, a byte past `limit_bytes` was written or a write makes no progress; then remove the
    file. The file is synced after every `FILL_SYNC_BYTES` written and again after a write the
    filesystem refused, before its free space is read: a refused write alone does not show that
    the data before it was committed, and that sync's own error, a delayed capacity refusal or an
    I/O error, is the fill's. Returns the `fill` record: the bytes every successful write
    accepted (a sync that fails afterwards does not unwrite them), why it stopped, the error and
    its stage, the space `statvfs` reported free when it stopped or why it could not, and any
    error closing or removing the file, each kept verbatim. Nothing is retried.

    `deadline`, an instant of `host.clock` (`FILL_SECONDS_MAX` after the start by default),
    bounds all of it (ADR-0173). No write starts within `FILL_CLEANUP_SECONDS` before it, and a
    write or sync that returns after that, whatever it returns, stops the fill as `deadline`:
    a capacity refusal that came too late verifies nothing. The calls run in a worker the caller
    waits for only until the deadline; one still blocked then is abandoned, `deadline` too, with
    its file kept and its cleanup unconfirmed, and the worker makes no further call but closing
    its descriptor."""
    host = host or Host()
    started = host.clock()
    if deadline is None:
        deadline = started + FILL_SECONDS_MAX
    path = os.path.join(root, ".af-perf-enforcement-%d" % os.getpid())
    progress = _Progress()
    worker = threading.Thread(target=_fill, name="af-perf-scratch-fill",
                              args=(progress, path, root, limit_bytes, host,
                                    deadline - FILL_CLEANUP_SECONDS, started))
    worker.daemon = True
    worker.start()
    host.wait(progress.done, deadline - host.clock())
    with progress.lock:
        if progress.record is None:
            progress.abandoned = True
            stage, written = progress.stage, progress.written
    if progress.abandoned:
        return {"written_bytes": written, "stopped": "deadline", "errno": None, "stage": None,
                "detail": None, "available_bytes_at_stop": None, "available_unknown": None,
                "elapsed_ms": int((host.clock() - started) * 1000),
                "cleanup": ("not confirmed: its %s call had not returned at the deadline; %s "
                            "is kept" % (stage, path))[:1024]}
    record = progress.record
    if host.clock() > deadline and record["stopped"] != "deadline":
        # Answered after the deadline: whatever it says, the deadline ended it.
        record.update(stopped="deadline", errno=None, stage=None, detail=None)
    record["elapsed_ms"] = int((host.clock() - started) * 1000)
    return record


def _fill(progress, path, root, limit_bytes, host, writes_end, started):
    """The fill's calls, in its worker: every call is made only while the caller still waits,
    and a write or sync answered at or after `writes_end` stops it as `deadline`."""
    record = {"written_bytes": 0, "stopped": None, "errno": None, "stage": None,
              "detail": None, "available_bytes_at_stop": None, "available_unknown": None,
              "elapsed_ms": 0, "cleanup": None}

    def stop(why, stage=None, error=None):
        record.update(stopped=why, stage=stage, errno=None, detail=None)
        if error is not None:
            record["errno"] = errno.errorcode.get(error.errno, "errno %s" % error.errno)
            record["detail"] = _error(error)

    def step(stage):
        """Enter `stage` unless the caller abandoned the fill."""
        with progress.lock:
            if progress.abandoned:
                return False
            progress.stage = stage
            progress.written = record["written_bytes"]
            return True

    def finish():
        with progress.lock:
            if not progress.abandoned:
                progress.record = record
        progress.done.set()

    def late():
        return host.clock() >= writes_end

    def failed(stage, error):
        """A call's error, unless it answered after its bound: then the deadline's."""
        if late():
            stop("deadline")
        else:
            stop("refused" if error.errno in CAPACITY_ERRNOS and stage != "open" else "error",
                 stage, error)

    if late() or not step("open"):
        stop("deadline")
        return finish()
    try:
        fd = host.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except OSError as error:
        failed("open", error)
        return finish()
    cleanup = []
    try:
        chunk = b"\0" * FILL_CHUNK
        unsynced = 0
        while record["stopped"] is None:
            if record["written_bytes"] > limit_bytes:
                stop("limit_exceeded")
                break
            if late() or not step("write"):
                stop("deadline")
                break
            try:
                count = host.write(fd, chunk)
            except OSError as error:
                failed("write", error)
                break
            if count > 0:
                record["written_bytes"] += count
                unsynced += count
            if late():
                stop("deadline")
                break
            if count <= 0:
                stop("no_progress", "write")
                break
            if unsynced < FILL_SYNC_BYTES:
                continue
            if not step("fsync"):
                stop("deadline")
                break
            unsynced = 0
            try:
                host.fsync(fd)
            except OSError as error:
                failed("fsync", error)
        if record["stopped"] == "refused" and record["stage"] == "write":
            # Commit what the refused write followed before the free space is read.
            if not step("fsync"):
                stop("deadline")
            else:
                try:
                    host.fsync(fd)
                    if late():
                        stop("deadline")
                except OSError as error:
                    failed("fsync", error)
        if step("statvfs"):
            try:
                info = host.statvfs(root)
                record["available_bytes_at_stop"] = info.f_bavail * info.f_frsize
            except OSError as error:
                record["available_unknown"] = _error(error)
    finally:
        # An abandoned fill only closes its descriptor: its file stays for the operator.
        kept = not step("close")
        for stage, call in (("close", lambda: host.close(fd)),
                            ("unlink", lambda: host.unlink(path))):
            if stage == "unlink" and (kept or not step(stage)):
                break
            try:
                call()
            except OSError as error:
                cleanup.append("%s: %s" % (stage, _error(error)))
    record["cleanup"] = "; ".join(cleanup)[:1024] or None
    record["elapsed_ms"] = int((host.clock() - started) * 1000)
    finish()


def fill_refusal(record, limit_bytes):
    """Why a fill does not verify a bounded filesystem, or None when it does: the filesystem
    itself refused more data for lack of space or quota, after real progress and within the
    limit, then reported less than two chunks free, and the fill file was removed."""
    written = record["written_bytes"]
    if record["stopped"] != "refused":
        what = {"error": "writing failed with %s at %s, which is not a capacity refusal"
                         % (record["detail"], record["stage"]),
                "no_progress": "a write accepted no bytes",
                "deadline": "the fill did not finish within its time bound",
                "limit_exceeded": "%d bytes were written without the filesystem refusing"
                                  % written}.get(record["stopped"], "the fill did not stop")
        why = "enforcement not verified: %s (%d bytes written)" % (what, written)
    elif written == 0:
        why = ("enforcement not verified: %s refused the first write; a full or refusing "
               "filesystem without progress proves no bound" % record["errno"])
    elif written > limit_bytes:
        why = "enforcement not verified: %d bytes were accepted, beyond the %d-byte limit" \
            % (written, limit_bytes)
    elif record["available_bytes_at_stop"] is None \
            or record["available_bytes_at_stop"] >= 2 * FILL_CHUNK:
        free = record["available_bytes_at_stop"]
        why = ("enforcement not verified: %s at %s, but the filesystem then reported %s, so "
               "the refusal is not shown to be its capacity" % (
                   record["errno"], record["stage"],
                   "%d bytes free" % free if free is not None
                   else "no free space (statvfs: %s)" % record["available_unknown"]))
    else:
        why = None
    if record["cleanup"] is not None:
        cleanup = "the fill file was not removed cleanly: " + record["cleanup"]
        why = cleanup if why is None else why + "; " + cleanup
    return None if why is None else why[:1024]


def docker_info(docker, timeout=TOOL_TIMEOUT_SECONDS):
    """The container runtime's identity, its query bounded by `timeout` seconds: at most the
    tool bound, less when a run's remaining deadline is shorter (ADR-0171)."""
    return _tool([docker, "info", "--format",
                  "{{.OSType}} {{.Architecture}} cgroup{{.CgroupVersion}} {{.ServerVersion}}"],
                 timeout=timeout)


def report(suite_doc, docker="docker", scratch=None, verify_enforcement=False):
    """The validated `af.perf-capabilities/2` report of this host."""
    system = host_platform()
    machine = platform.machine()
    arch = {"arm64": "aarch64", "amd64": "x86_64"}.get(machine.lower(), machine)
    tools = {"git": _tool(["git", "--version"]), "rustc": _tool(["rustc", "-V"]),
             "docker_client": _tool([docker, "version", "--format", "{{.Client.Version}}"]),
             "docker_server": docker_info(docker)}
    trusted = {"status": "available", "reasons": []}
    if system != "macos":
        trusted = {"status": "unavailable",
                   "reasons": ["trusted local execution is the explicit macOS capability; "
                               "Linux samples run in containers"]}
    limits = sorted(set(env["scratch_limit_bytes"] for env in suite_doc["environments"].values()
                        if env["containment"] == "linux_container")) or [2 << 30]
    scratch_record = scratch_capability(scratch, limits[0], verify_enforcement, system)
    reasons = []
    if tools["docker_server"]["unavailable"]:
        reasons.append("no container runtime: " + tools["docker_server"]["unavailable"])
    else:
        fields = tools["docker_server"]["identity"].split(" ")
        if fields[0] != "linux":
            reasons.append("the container runtime runs %s containers, not Linux" % fields[0])
        if len(fields) > 2 and fields[2] != "cgroup2":
            reasons.append("the container runtime uses %s, not cgroup v2" % fields[2])
    if scratch_record["capability"]["status"] != "available":
        reasons.append("no verified disk-bounded scratch: "
                       + "; ".join(scratch_record["capability"]["reasons"]))
    container = {"status": "unavailable" if reasons else "available", "reasons": reasons}
    environments = {}
    for env_id, env in sorted(suite_doc["environments"].items()):
        why = []
        if env["os"] != system:
            why.append("declares %s, this host is %s" % (env["os"], system))
        if env["arch"] != arch:
            why.append("declares %s, this host is %s" % (env["arch"], arch))
        base = container if env["containment"] == "linux_container" else trusted
        why += base["reasons"]
        if env["state"] != "supported":
            why.append("registry: " + (env["pending"] or "not yet executable"))
        environments[env_id] = {"status": "unavailable" if why else "available",
                                "reasons": [reason[:1024] for reason in why][:32]}
    doc = {"schema": contracts.CAPABILITIES_SCHEMA,
           "host": {"platform": system, "arch": arch or "unknown",
                    "kernel": (platform.release() or "unknown")[:256],
                    "python": platform.python_version()},
           "tools": tools,
           "trusted_local": {"capability": trusted,
                             "scope": "discovery_and_nonspawning_library_cases",
                             "same_user_isolation": "not_provided"},
           "linux_container": {"capability": container, "scratch": scratch_record},
           "environments": environments}
    return contracts.validate_capabilities(doc)
