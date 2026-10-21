"""The trusted side of the native probe: requests, execution and the golden oracle (ADR-0163).

The driver builds each probe request from the authority registry, runs one probe process under
the authority-built supervisor (`perflab.supervise`, ADR-0166) in a private per-invocation
scratch namespace (`Namespace`), and judges what it wrote against the authority's golden
entries. A probe's own `passed` is necessary, never sufficient: identity, fixture digest,
observed digest and count must all equal the golden entry, which `perflab.reference` derived
from reference bytes without running any product, and which `load` checks against that
generator before either command uses it. Every failure is attributed to the side whose probe
produced it, with the sample contract's failure kinds.

`cli.py probe` is a diagnostic single sample in explicitly trusted local mode: the supervisor
runs the probe in its own process group, which is not Linux namespace containment, so only
cases that spawn nothing run here; spawning cases require a Linux sample container.
"""
import os
import secrets
import stat
import sys

from . import contracts, limits, reference, supervise
from . import suite as suite_module
from .values import ContractError, loads_strict, require

CHECK_SCHEMA = "af.perf-probe-check/1"
EXIT_REFUSED = 2
REQUIRED_METRICS = ("wall_ms", "setup_ms")
# Metrics a contract's successful message must also carry.
CONTRACT_METRICS = {"persistence_fs_v1": ("logical_bytes",)}
DETAIL_MAX = 4096
# A cleanup diagnostic appended to a failure never takes more than this, so the original
# diagnostic always keeps the rest; the complete cleanup report goes to stderr, bounded too.
CLEANUP_NOTE_MAX = 1024
CLEANUP_STDERR_MAX = 16 * 1024


class Unsupported(ContractError):
    """The sample needs a containment this mode does not provide; nothing was launched."""


def golden_path(suite_path, doc):
    return os.path.join(os.path.dirname(os.path.abspath(suite_path)), doc["probe"]["golden"])


def load_golden(path):
    with open(path, "rb") as stream:
        data = stream.read(limits.MAX_JSON_BYTES + 1)
    return contracts.read_json(data, "golden", path)


def check_coverage(doc, golden, where="golden"):
    """Every native instance, and every seed one reads (ADR-0170), has exactly one entry with
    its identity, and nothing else does."""
    for key, declared in (("entries", [(item, contract_id) for item, contract_id, _
                                       in reference.native_instances(doc)]),
                          ("seeds", reference.native_seeds(doc))):
        expected = dict((item["instance_id"], (item, contract_id))
                        for item, contract_id in declared)
        present = dict((entry["instance_id"], entry) for entry in golden[key])
        missing = sorted(set(expected) - set(present))
        extra = sorted(set(present) - set(expected))
        require(not missing, where + ".%s: no golden entry for %s" % (key, ", ".join(missing)))
        require(not extra, where + ".%s: golden entries for undeclared %s" % (
            key, ", ".join(extra)))
        for ident, (item, contract_id) in sorted(expected.items()):
            entry = present[ident]
            require((entry["case_id"], entry["contract_id"], entry["parameters"])
                    == (item["case_id"], contract_id, item["parameters"]),
                    where + ".%s.%s: identity differs from the registry" % (key, ident))
    constraints = doc["product_constraints"]
    require(golden["constants"]["task_lease_ms"] == constraints["task_lease_ms"]
            and golden["constants"]["artifact_envelope_cap_bytes"]
            == constraints["artifact_envelope_cap_bytes"],
            where + ".constants: differ from the registry's product constraints")
    return golden


def check_reference(doc, golden, where="golden"):
    """The committed golden document is exactly the reference generator's output."""
    require(golden == reference.golden_document(doc),
            where + ": is not what perflab.reference derives for this registry; regenerate "
            "with `python3 scripts/performance/cli.py golden --write` and review the change")
    return golden


def _contract(doc, instance_id):
    for item, contract_id, repetitions in reference.native_instances(doc):
        if item["instance_id"] == instance_id:
            return item, contract_id, repetitions
    raise ContractError("instance %s has no available native oracle in this registry"
                        % instance_id)


def seed_of(doc, instance_id):
    """The seed a native instance reads (`suite.seed_of`), or None."""
    item, _, _ = _contract(doc, instance_id)
    if item["case_id"] == doc["health"]["workload"]:
        return None
    return suite_module.seed_of(doc, item)


def _writer(doc, seed_id):
    for item, contract_id in reference.native_seeds(doc):
        if item["instance_id"] == seed_id:
            return item, contract_id
    raise ContractError("seed %s is read by no native instance of this registry" % seed_id)


def request_for(doc, instance_id, scratch, seed=None):
    """The probe request of one native instance, from the authority registry alone; `seed` is
    the sample's private copy of the seed the instance reads, exactly when it reads one."""
    item, contract_id, repetitions = _contract(doc, instance_id)
    reads = seed_of(doc, instance_id) is not None
    require(reads == (seed is not None), "%s %s; the request %s" % (
        instance_id, "reads a seed" if reads else "reads no seed",
        "names none" if seed is None else "names one"))
    request = {"schema": contracts.PROBE_REQUEST_SCHEMA, "case_id": item["case_id"],
               "instance_id": item["instance_id"], "contract_id": contract_id,
               "parameters": item["parameters"], "repetitions": repetitions,
               "scratch": scratch, "seed": seed}
    return contracts.validate_probe_request(request)


def seed_request(doc, seed_id, scratch):
    """The request of a seed's writer contract writing in `scratch` (ADR-0170)."""
    item, contract_id = _writer(doc, seed_id)
    request = {"schema": contracts.PROBE_REQUEST_SCHEMA, "case_id": item["case_id"],
               "instance_id": item["instance_id"], "contract_id": contract_id,
               "parameters": item["parameters"], "repetitions": 1, "scratch": scratch,
               "seed": None}
    return contracts.validate_probe_request(request)


def is_seed_request(doc, request):
    return any(item["instance_id"] == request["instance_id"]
               for item, _ in reference.native_seeds(doc))


def check_request(doc, request, where="perf-probe-request"):
    """A recorded request is exactly the registry's declaration of its instance or seed.

    Case, contract, typed parameter assignment, repetitions and whether a seed is read must all
    equal what `request_for` (or `seed_request`) builds; a request that differs describes an
    undeclared execution and is refused before any output is judged, whatever that output
    says."""
    contracts.validate_probe_request(request, where)
    if is_seed_request(doc, request):
        require(request["seed"] is None, where + ": a seed writer reads no seed")
        declared = seed_request(doc, request["instance_id"], request["scratch"])
    else:
        reads = seed_of(doc, request["instance_id"]) is not None
        require(reads == (request["seed"] is not None),
                where + ": %s %s" % (request["instance_id"], "reads a seed; the request names "
                                     "none" if reads else "reads no seed; the request names one"))
        declared = request_for(doc, request["instance_id"], request["scratch"],
                               request["seed"])
    wrong = [key for key in ("case_id", "contract_id", "parameters", "repetitions")
             if contracts.canonical_bytes(request[key]) != contracts.canonical_bytes(declared[key])]
    require(not wrong, where + ": differs from the registry's declaration of %s in %s"
            % (request["instance_id"], ", ".join(wrong)))
    return request


def _metrics_allowed(doc, case_id):
    if case_id == doc["health"]["workload"] or case_id not in doc["cases"]:
        # The sentinel and a seed writer report the region and its setup only.
        return dict((key, doc["metrics"][key]["unit"]) for key in REQUIRED_METRICS)
    case = doc["cases"][case_id]
    keys = [case["primary_metric"]] + case["optional_metrics"]
    return dict((key, doc["metrics"][key]["unit"]) for key in keys)


def _result(request, status, failure=None, metrics=None, oracle_status="not_run",
            observed=None):
    return {"schema": CHECK_SCHEMA, "instance_id": request["instance_id"],
            "contract_id": request["contract_id"], "status": status,
            "failure": failure, "metrics": metrics or {},
            "oracle": {"status": oracle_status, "observed_digest": observed}}


def _failed(request, kind, detail, metrics=None, oracle_status="not_run", observed=None):
    return _result(request, "failed", {"kind": kind, "detail": bounded(detail, DETAIL_MAX)},
                   metrics, oracle_status, observed)


def bounded(text, limit):
    """`text` cut to at most `limit` characters, saying how much was cut."""
    if len(text) <= limit:
        return text
    marker = " [%d more characters]"
    cut = limit - len(marker % len(text))
    cut = max(cut, 0)
    return text[:cut] + marker % (len(text) - cut)


def _logical_bytes(request, metrics):
    """None when a persistence message's logical bytes hold, else what is wrong with them."""
    value = metrics["logical_bytes"]["value"]
    if not value.isdigit() or value.startswith("0"):
        return "logical_bytes %s is not a canonical positive integer" % value
    expected = reference.persistence_logical_bytes(request["parameters"]["operation"])
    if expected is not None and value != str(expected):
        return "logical_bytes %s differs from the reference's %d" % (value, expected)
    return None


def judge(doc, golden, request, returncode, stdout):
    """Judge one probe execution of `request` against the golden entry of its instance, or of
    its seed for a seed writer's request (ADR-0170)."""
    check_request(doc, request)
    key = "seeds" if is_seed_request(doc, request) else "entries"
    entry = dict((item["instance_id"], item) for item in golden[key]).get(
        request["instance_id"])
    require(entry is not None, "golden: no %s entry for %s" % (key, request["instance_id"]))
    if returncode == EXIT_REFUSED:
        return _failed(request, "launch_failure", "the probe refused its request")
    if not stdout:
        return _failed(request, "nonzero_exit",
                       "the probe wrote no message (exit %s)" % returncode)
    if len(stdout) > limits.RECORD_BYTES_MAX or stdout.count(b"\n") != 1 \
            or not stdout.endswith(b"\n"):
        return _failed(request, "oracle_mismatch",
                       "the probe output is not exactly one bounded line")
    try:
        message = contracts.read_probe_line(stdout)
    except ContractError as error:
        return _failed(request, "oracle_mismatch", "malformed probe message: %s" % error)
    identity = ("case_id", "instance_id", "contract_id", "parameters")
    wrong = [key for key in identity if message[key] != request[key]
             or (key == "parameters" and any(type(message[key][name]) is not type(value)
                                             for name, value in request[key].items()))]
    if wrong:
        return _failed(request, "oracle_mismatch",
                       "the message names a different " + ", ".join(wrong))
    allowed = _metrics_allowed(doc, request["case_id"])
    metrics = message["metrics"]
    undeclared = sorted(key for key, value in metrics.items()
                        if allowed.get(key) != value["unit"])
    if undeclared:
        return _failed(request, "oracle_mismatch", "undeclared or mis-united metrics "
                       + ", ".join(undeclared))
    correctness = message["correctness"]
    if correctness["status"] == "failed":
        kind = ("lease_expired" if correctness["detail"].startswith("lease_expired:")
                else "nonzero_exit")
        return _failed(request, kind, correctness["detail"], metrics)
    if returncode != 0:
        return _failed(request, "nonzero_exit",
                       "the probe passed its own checks but exited %s" % returncode, metrics)
    required = REQUIRED_METRICS + CONTRACT_METRICS.get(request["contract_id"], ())
    missing = [key for key in required if key not in metrics]
    if missing:
        return _failed(request, "missing_required_metric", "missing " + ", ".join(missing),
                       metrics)
    wrong = _logical_bytes(request, metrics) if "logical_bytes" in required else None
    if wrong:
        return _failed(request, "oracle_mismatch", wrong, metrics, "failed",
                       correctness["digest"])
    mismatched = [key for key in ("fixture_digest", "digest", "count")
                  if (message["fixture_digest"] if key == "fixture_digest"
                      else correctness[key]) != entry[key]]
    if message["contract_id"] != entry["contract_id"]:
        mismatched.insert(0, "contract_id")
    if mismatched:
        return _failed(request, "oracle_mismatch",
                       "differs from the golden entry in " + ", ".join(mismatched), metrics,
                       "failed", correctness["digest"])
    return _result(request, "passed", None, metrics, "passed", correctness["digest"])


def capability(doc, instance_id):
    """What a single driver sample is and is not: never a paired Performance Sample, never
    Linux namespace containment, and never an execution of a case that spawns processes."""
    item, _, _ = _contract(doc, instance_id)
    case = doc["cases"].get(item["case_id"])
    contained = case is not None and case["requires_linux_containment"]
    return {"scope": "diagnostic_single_sample", "host_platform": sys.platform,
            "containment": "trusted_local_process_group",
            "same_user_isolation": "not_provided",
            "linux_containment": "required_not_provided" if contained else "not_required",
            "speed_claim": "none"}


def check_timeout(timeout_seconds):
    """A sample timeout is a positive whole number of seconds within the hard maximum."""
    require(type(timeout_seconds) is int
            and 1 <= timeout_seconds <= limits.SAMPLE_TIMEOUT_SECONDS_MAX,
            "timeout: %r is not a whole number of seconds from 1 to %d"
            % (timeout_seconds, limits.SAMPLE_TIMEOUT_SECONDS_MAX))
    return timeout_seconds


NAMESPACE_PREFIX = "perf-probe-"
# The directory a probe creates below its scratch and writes only below (the probe's WORK_DIR).
WORK_DIR = "probe-work"
TOMBSTONE_PREFIX = ".perf-removing-"
_DIRECTORY = os.O_RDONLY | os.O_DIRECTORY
_HELD = _DIRECTORY | os.O_NOFOLLOW
_FILE = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK


def _identity(st):
    return (st.st_dev, st.st_ino)


def _listing(fd):
    """The names in the directory held by `fd`, read through a new open description of it.

    A held descriptor already enumerated once did not see entries created afterwards on a
    supported Linux environment (Python 3.9 and 3.11 on a 6.10 kernel), even though
    `os.listdir` rewinds the description it shares. `"."` opened relative to the handle is the
    held directory itself, whatever its name is now, and enumerates its current entries."""
    fresh = os.open(".", _DIRECTORY, dir_fd=fd)
    try:
        require(_identity(os.fstat(fresh)) == _identity(os.fstat(fd)),
                "a directory's own entry is not the held directory")
        return sorted(os.listdir(fresh))
    finally:
        os.close(fresh)


class Namespace:
    """A per-invocation scratch namespace in explicitly trusted local mode, held by a handle.

    The ownership boundary is the caller's private parent directory: it must be owned by this
    user and writable by nobody else, so no other user can create, rename or remove anything in
    it. Processes of this same user are inside the trust of local mode and are not excluded:
    POSIX lets such a process replace an entry between any check and use, and in particular
    swap an indistinguishable empty 0700 directory in before the handle exists; nothing the
    driver can observe tells the two apart, so no claim of exclusive ownership against such a
    process is made (`same_user_isolation: not_provided`). Linux samples do not use this
    class: their scratch is owned and cleared inside the sample container's PID namespace.

    The namespace is a new directory with a random name, created with mode 0700 before any
    probe exists. Its handle is opened without following symlinks and must be the empty 0700
    directory of this user now at that name; anything else there is refused and left in
    place, and nothing launches. The probe is given only the namespace's path; the parent and
    every other entry in it are never touched.

    `remove` works through handles only: every directory is enumerated through a new open
    description of its handle, a symlink is unlinked and never followed, and an entry is
    removed only after it is renamed to a fresh random tombstone name in the same held
    directory and the tombstone is verified to be the very entry that was held or observed. An
    entry that is not, is renamed back if its name is free and left in place, and the cleanup
    is reported failed: no replacement is removed, and a passed sample becomes a failed one. A
    removed directory is confirmed gone through its own handle. The only residue of POSIX's
    missing removal by handle is the moment between the tombstone's verification and its
    `rmdir`: a same-user process that found the random tombstone name exactly then could have
    its empty directory removed, and the confirmation reports that the held directory was not
    the one removed, so the cleanup is uncertain and the sample fails.

    `boundary`, for tests only, is called with a stage and a path: "created" between the
    `mkdir` and the handle, "child_cleared" when a subdirectory's contents are gone and before
    its removal, "cleared" between the namespace's contents' removal and its own, and
    "tombstoned" between a directory tombstone's verification and its `rmdir`."""

    def __init__(self, parent, boundary=None):
        self._boundary = boundary or (lambda stage, path: None)
        self.parent = os.path.realpath(parent)
        self.name = NAMESPACE_PREFIX + secrets.token_hex(16)
        self.path = os.path.join(self.parent, self.name)
        self._fd = None
        self._parent_fd = os.open(self.parent, _DIRECTORY)
        try:
            self._create()
        except BaseException:
            self._release()
            raise

    def _create(self):
        parent = os.fstat(self._parent_fd)
        require(parent.st_uid == os.geteuid() and not parent.st_mode & 0o022,
                "scratch: %s must be owned by this user and writable by no one else; that "
                "private directory is the trusted local ownership boundary" % self.parent)
        os.mkdir(self.name, 0o700, dir_fd=self._parent_fd)
        self._boundary("created", self.path)
        refused = ("scratch: %s is not the empty private directory this driver created; it "
                   "was left in place and nothing was launched" % self.path)
        try:
            self._fd = os.open(self.name, _HELD, dir_fd=self._parent_fd)
        except OSError as error:
            raise ContractError("%s (%s)" % (refused, error.strerror))
        held = os.fstat(self._fd)
        named = os.stat(self.name, dir_fd=self._parent_fd, follow_symlinks=False)
        require(stat.S_ISDIR(held.st_mode) and _identity(held) == _identity(named)
                and held.st_uid == os.geteuid() and stat.S_IMODE(held.st_mode) == 0o700
                and not _listing(self._fd), refused)
        self.identity = _identity(held)

    def _release(self):
        for fd in (self._fd, self._parent_fd):
            if fd is not None:
                os.close(fd)
        self._fd = self._parent_fd = None

    def keep(self):
        """Leave the namespace in place, its handles released, and return its path: a process
        of the sample whose cleanup was not confirmed may still be live in it, so nothing of it
        is removed and the operator recovers it (ADR-0171)."""
        self._release()
        return self.path

    def remove(self):
        """Remove the namespace; the problems found, empty only when removal is confirmed."""
        problems = []
        try:
            _clear(self._fd, self.path, problems, self._boundary)
            self._boundary("cleared", self.path)
            if not problems:
                _remove_directory(self._parent_fd, self.name, self._fd, self.identity,
                                  self.path, problems, self._boundary)
        except OSError as error:
            problems.append("could not remove %s: %s" % (self.path, error.strerror))
        finally:
            self._release()
        return problems


def _tombstone(parent_fd, name, identity, where, problems):
    """Rename `name` to a fresh tombstone in the same held directory and verify it is the entry
    with `identity`; the tombstone name, or None after restoring a replacement."""
    tomb = TOMBSTONE_PREFIX + secrets.token_hex(16)
    try:
        os.rename(name, tomb, src_dir_fd=parent_fd, dst_dir_fd=parent_fd)
    except FileNotFoundError:
        problems.append("%s vanished before its removal" % where)
        return None
    except OSError as error:
        problems.append("could not remove %s: %s; it was left in place"
                        % (where, error.strerror))
        return None
    try:
        moved = os.stat(tomb, dir_fd=parent_fd, follow_symlinks=False)
    except OSError as error:
        problems.append("%s moved again while it was removed (%s); left in place"
                        % (where, error.strerror))
        return None
    if _identity(moved) == identity:
        return tomb
    # Not the entry that was checked: a replacement. It is put back, never removed.
    try:
        os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
        restored = False
    except FileNotFoundError:
        os.rename(tomb, name, src_dir_fd=parent_fd, dst_dir_fd=parent_fd)
        restored = True
    problems.append("%s was replaced; the entry now there was %s and nothing of it was "
                    "removed" % (where, "left in place" if restored else
                                 "kept at %s because the name was taken again"
                                 % os.path.join(os.path.dirname(where), tomb)))
    return None


def _remove_directory(parent_fd, name, held_fd, identity, where, problems, boundary):
    """Remove the emptied directory held by `held_fd` from its entry `name`, never another."""
    tomb = _tombstone(parent_fd, name, identity, where, problems)
    if tomb is None:
        return
    boundary("tombstoned", os.path.join(os.path.dirname(where), tomb))
    try:
        os.rmdir(tomb, dir_fd=parent_fd)
    except OSError as error:
        problems.append("could not remove %s: %s; it was left in place as %s"
                        % (where, error.strerror, tomb))
        return
    # Removal is confirmed only when the held directory can no longer gain an entry.
    try:
        os.mkdir(".removed", 0o700, dir_fd=held_fd)
    except FileNotFoundError:
        return
    os.rmdir(".removed", dir_fd=held_fd)
    problems.append("%s was not the directory removed at its tombstone: a same-user process "
                    "replaced it within the removal, so cleanup is uncertain and the held "
                    "directory remains" % where)


def _clear(fd, where, problems, boundary):
    """Remove every entry below the directory held by `fd`, through descriptors only.

    Nothing is followed: a symlink is unlinked, a subdirectory is opened relative to its
    parent's handle without following symlinks and must be the directory just observed at that
    name, and every entry goes through a verified tombstone."""
    try:
        names = _listing(fd)
    except OSError as error:
        problems.append("could not list %s: %s" % (where, error.strerror))
        return
    for name in names:
        path = os.path.join(where, name)
        try:
            entry = os.stat(name, dir_fd=fd, follow_symlinks=False)
            if stat.S_ISDIR(entry.st_mode):
                child = os.open(name, _HELD, dir_fd=fd)
                try:
                    if _identity(os.fstat(child)) != _identity(entry):
                        problems.append("%s changed while it was removed; left in place" % path)
                        continue
                    before = len(problems)
                    _clear(child, path, problems, boundary)
                    boundary("child_cleared", path)
                    if len(problems) == before:
                        _remove_directory(fd, name, child, _identity(entry), path, problems,
                                          boundary)
                finally:
                    os.close(child)
            else:
                _remove_file(fd, name, entry, path, problems)
        except OSError as error:
            problems.append("could not remove %s: %s" % (path, error.strerror))


def _remove_file(fd, name, entry, where, problems):
    """Unlink a non-directory only through a verified tombstone; a regular file is held open
    and its removal confirmed by its link count."""
    held = None
    if stat.S_ISREG(entry.st_mode):
        try:
            held = os.open(name, _FILE, dir_fd=fd)
        except OSError:
            held = None
        if held is not None and _identity(os.fstat(held)) != _identity(entry):
            os.close(held)
            problems.append("%s changed while it was removed; left in place" % where)
            return
    try:
        tomb = _tombstone(fd, name, _identity(entry), where, problems)
        if tomb is None:
            return
        os.unlink(tomb, dir_fd=fd)
        if held is not None and os.fstat(held).st_nlink != 0:
            problems.append("%s still has a link after its removal; another name of it "
                            "remains" % where)
    finally:
        if held is not None:
            os.close(held)


def _kept(namespace):
    return ("the private scratch %s was kept: the sample's cleanup was not confirmed, so a "
            "process of it may still be live there" % namespace.keep())


def _with_cleanup(detail, note):
    """The original diagnostic first, then a bounded cleanup note; both fit the detail cap."""
    note = bounded(note, CLEANUP_NOTE_MAX)
    return bounded(detail, DETAIL_MAX - len(note) - 2) + "; " + note


def _cleanup_failed(request, result, path, problems):
    """A sample whose private scratch was not confirmed removed never reports success. The
    original diagnostic is kept first; the cleanup note is bounded and appended."""
    note = "the private scratch %s was not removed: %s" % (path, "; ".join(problems))
    if result["status"] == "passed":
        failed = _failed(request, "lifecycle_violation",
                         _with_cleanup("the sample passed", note), result["metrics"],
                         result["oracle"]["status"], result["oracle"]["observed_digest"])
        for key in ("capability", "supervision"):
            if key in result:
                failed[key] = result[key]
        return failed, note
    result["failure"]["detail"] = _with_cleanup(result["failure"]["detail"], note)
    return result, note


def judge_supervised(doc, golden, request, supervised, containment):
    """Judge one supervised probe execution: lifecycle and termination first, then output."""
    kind, detail, _ = supervise.outcome(supervised, containment)
    if kind is not None:
        return _failed(request, kind, detail)
    return judge(doc, golden, request, supervise.returncode(supervised.result),
                 supervised.stdout)


def sample(doc, golden, supervisor, binary, instance_id, scratch_root, timeout_seconds,
           boundary=None):
    """One trusted local probe sample under the authority supervisor, in a private namespace
    below `scratch_root`.

    A case that spawns processes needs Linux namespace containment and is refused before
    anything is created. The namespace is removed afterwards through its handle; a cleanup that
    is not confirmed fails the sample, whatever the probe did, and is written to the returned
    stderr after the probe's own. A supervised execution whose own cleanup is uncertain leaves
    the namespace in place, never removed beneath a process that may still be live (ADR-0171)."""
    check_timeout(timeout_seconds)
    item, _, _ = _contract(doc, instance_id)
    case = doc["cases"].get(item["case_id"])
    if case is not None and case["requires_linux_containment"]:
        raise Unsupported("%s spawns processes and requires Linux namespace containment; "
                          "trusted local mode runs only discovery and nonspawning library "
                          "cases" % instance_id)
    namespace = Namespace(scratch_root, boundary)
    uncertain = []

    def supervised_probe(request):
        order = supervise.request(
            [binary], supervise.probe_environment(namespace.path), namespace.path,
            contracts.canonical_bytes(request).decode("utf-8"), timeout_seconds * 1000,
            limits.RECORD_BYTES_MAX, doc["limits"]["sample_log_bytes"])
        supervised = supervise.launch([supervisor], order)
        if not supervise.cleanup_confirmed(supervised.result):
            uncertain.append(supervised.result["lifecycle"]["detail"])
        return supervised

    try:
        scratch, seed, written = namespace.path, None, None
        reads = seed_of(doc, instance_id)
        if reads is not None:
            # A diagnostic sample writes its own seed through the same probe and reads it once:
            # the writer's check is judged against the golden seed entry first (ADR-0170).
            seed_root = os.path.join(namespace.path, "seed")
            scratch = os.path.join(namespace.path, "probe")
            for path in (seed_root, scratch):
                os.mkdir(path, 0o700)
            writer = seed_request(doc, reads["seed_id"], seed_root)
            written = judge_supervised(doc, golden, writer, supervised_probe(writer),
                                       "process_group")
            seed = os.path.join(seed_root, WORK_DIR)
        request = request_for(doc, instance_id, scratch, seed)
        if written is not None and written["status"] != "passed":
            result = _failed(request, "launch_failure", "the seed %s was not written, so "
                             "nothing was measured: %s: %s" % (
                                 reads["seed_id"], written["failure"]["kind"],
                                 written["failure"]["detail"]))
            supervised = None
        else:
            supervised = supervised_probe(request)
            result = judge_supervised(doc, golden, request, supervised, "process_group")
        result["capability"] = capability(doc, instance_id)
        if supervised is not None:
            result["supervision"] = supervise.summary(supervised)
    except Exception as error:
        if getattr(error, "uncertain", False) or uncertain:
            raise ContractError(_with_cleanup(str(error), _kept(namespace)))
        problems = namespace.remove()
        if problems:
            raise ContractError(_with_cleanup(str(error), "and the private scratch %s was not "
                                              "removed: %s" % (namespace.path,
                                                               "; ".join(problems))))
        raise
    stderr = supervised.stderr if supervised is not None else b""
    if uncertain:
        # The failed lifecycle already fails the sample; its scratch stays for recovery.
        note = _kept(namespace)
        return result, stderr + ("perf-probe driver: %s\n" % bounded(
            note, CLEANUP_STDERR_MAX)).encode("utf-8", "replace")
    problems = namespace.remove()
    if problems:
        result, note = _cleanup_failed(request, result, namespace.path, problems)
        stderr += ("perf-probe driver: %s\n" % bounded(note, CLEANUP_STDERR_MAX)).encode(
            "utf-8", "replace")
    return result, stderr


def timeout_for(doc, instance_id):
    item, _, _ = _contract(doc, instance_id)
    if item["case_id"] == doc["health"]["workload"]:
        return 30
    return doc["cases"][item["case_id"]]["sample_timeout_seconds"]


def read_request(path):
    with open(path, "rb") as stream:
        data = stream.read(limits.RECORD_BYTES_MAX + 1)
    require(len(data) <= limits.RECORD_BYTES_MAX, path + ": a request exceeds 64 KiB")
    return contracts.validate_probe_request(loads_strict(data, path, limits.RECORD_BYTES_MAX),
                                            path)


def load(suite_path):
    """The validated registry and its committed golden document, covered and exactly what the
    independent reference generator derives; only then may it judge either side."""
    loaded = suite_module.load(suite_path)
    golden = load_golden(golden_path(suite_path, loaded.doc))
    check_coverage(loaded.doc, golden)
    check_reference(loaded.doc, golden)
    return loaded.doc, golden
