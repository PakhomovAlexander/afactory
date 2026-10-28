"""Execute exactly one frozen shard of a plan (package p3b, ADR-0168, ADR-0169, ADR-0170).

`run_shard` takes a validated `run.json`, the verified artifacts of its build and one shard
index, and runs that shard's planned slots in their planned order on this host: the start
sentinel pairs, each case's two-pair AB/BA warmup and measured blocks, then the end sentinel
pairs. Nothing is rebuilt, retried, replaced, dropped or reordered, and no count changes with an
outcome. Every sample runs under the authority supervisor in the plan's environment: explicitly
trusted local mode on macOS (a process group, cases declared without Linux containment) or a
pinned Linux sample container per sample (a PID namespace, with only the per-sample directory of
a verified disk-bounded scratch filesystem writable). Every binary is rehashed against the build
manifest immediately before its launch.

The machine's one coordination lock (`perflab.hostlock`) is taken before anything shared is
touched, the run directory and the scratch filesystem's enforcement fill included, and held
until the terminal record is written. Large fixtures are seeded: before a side's first sample
that needs a seed (a CLI seed of `cli.toml`, or the native seed a library probe reads), that
side's own writers write it once under the supervisor; it is sealed, verified around every
sample and copied into each sample's own scratch in the sample's untimed setup
(`perflab.seeds`).

The run's one execution start and absolute total deadline come from its verified build
(ADR-0170): no shard resets them. A shard refuses before its preflight once that deadline has
passed, and its own deadline is the earliest of the run's, a caller's tighter one and its
sampling deadline. The run's deadline, tightened by a caller's, is carried through the
preflight itself (ADR-0171): every Git read, the container runtime query and the scratch
filesystem's enforcement fill is bounded by the time left before it less the terminal reserve,
and none starts once nothing is left. The scratch check is bounded by one instant fixed before
its first metadata step, which no later step resets (ADR-0173). That deadline is read from `builds.json` before any
artifact is rehashed, and every artifact hash, binary rehash and seed read leaves the reserve
while it runs (ADR-0172). A seed is admitted only once its writer is known to have ended,
through descriptors that never follow a symlink and within its declared bounds.

The shard writes one bundle below `<run>/shards/<index>/`: `samples.jsonl`, appended record by
record, `logs/` with the capped output of failed samples, and, whatever happened, a terminal
`shard.json` and the bundle manifest. A cleanup that is not confirmed, a supervisor that did not
reply, a changed binary, a scratch filesystem not restored exactly, or container limits other
than the environment's stop the shard (`failed`); the sampling or total deadline, checked
against each sample's worst case before it starts, or a termination signal ends it
(`interrupted`). Slots never started are counted as unstarted; nothing is recorded for them, so
the comparator reports them missing and the run incomplete.
"""
from fractions import Fraction
import os
import platform
import shutil
import signal
import tempfile
import time

from . import budget, builds, bundle, capability, clicases, contracts, gitrefs, host, hostlock
from . import limits
from . import oracle, planning, schedule, seeds, supervise
from .values import ContractError, canonical_bytes, document_bytes, format_decimal, require
from .values import sha256_bytes

SHARD_DIR = "shards"
SAMPLES = "samples.jsonl"
SHARD_FILE = "shard.json"
LOGS = "logs"
RUN_FILES = ("run.json", "builds.json")
TRUSTED_LOCAL = "trusted_local"
LINUX_CONTAINER = "linux_container"
GRACE_MS = 5000
# A sample's worst case beyond its deadlines: supervisor startup, verification, framing and,
# for a container, its creation and verified removal.
SAMPLE_MARGIN_SECONDS = {TRUSTED_LOCAL: 15, LINUX_CONTAINER: 90}
SETUP_TIMEOUT_MS = limits.SAMPLE_TIMEOUT_SECONDS_MAX * 1000
# Publishing a written seed: one copy within the scratch filesystem's bound.
SEED_PUBLISH_TIMEOUT_MS = limits.SAMPLE_TIMEOUT_SECONDS_MAX * 1000
# A native seed's writer: the side's probe running its writer contract (ADR-0170).
SEED_WRITER_TIMEOUT_MS = limits.SAMPLE_TIMEOUT_SECONDS_MAX * 1000
# Kept free before the deadline for the terminal record: seed removal, shard.json and manifest.
FINISH_RESERVE_SECONDS = 30
# How a native seed writer's failed check is recorded on its seed.
SEED_FAILURES = {"nonzero_exit": "fixture_failed", "lease_expired": "fixture_failed",
                 "missing_required_metric": "oracle_mismatch"}
SEED_LOG_BYTES = 64 * 1024
# Below a seeded library sample's scratch: the probe's own empty scratch and its seed copy, whose
# root holds the writer's work directory (ADR-0170).
NATIVE_PROBE_SCRATCH = "probe"
NATIVE_SEED_COPY = "seed/" + oracle.WORK_DIR
# Measure the frozen product, with no self-install or detached release update check in
# either fixture setup or the timed CLI command. Descendant refusal remains unchanged.
CLI_ENVIRONMENT = clicases.ENVIRONMENT
# The calibration loop's control, named only for a preparation or slowdown member (ADR-0181).
INJECTION_ENV = "AF_PERF_CALIBRATION_ITERATIONS"
CONTAINER_BINARIES = {"af": "/perf/bin/af", "probe": "/perf/bin/perf-probe"}


class Unavailable(ContractError):
    """This host cannot run the shard as frozen; nothing was sampled or substituted."""

    def __init__(self, reason, detail, cleanup_confirmed=True):
        ContractError.__init__(self, detail)
        self.reason = reason
        self.cleanup_confirmed = cleanup_confirmed


class Stop(Exception):
    """The shard must stop after the current slot: why, its terminal status, the slot's record
    when one was made, and whether a process the shard started may still be live (`retain`),
    so nothing that process could reach is removed (ADR-0171)."""

    def __init__(self, reason, detail, status="failed", record=None, retain=False):
        Exception.__init__(self, detail)
        self.reason = reason
        self.detail = detail[:4096]
        self.status = status
        self.record = record
        self.retain = retain

    def with_record(self, record):
        return Stop(self.reason, self.detail, self.status, record, self.retain)


class Options(object):
    """Everything a shard execution takes, as given on the command line."""

    def __init__(self, **values):
        self.plan = None
        self.build = None
        self.shard = None
        self.run_dir = None
        self.repo = "."
        self.mode = None
        self.scratch = None
        self.docker = "docker"
        self.image = None
        self.scratch_fs = None
        self.uid = os.getuid()
        self.gid = os.getgid()
        self.deadline_unix_ms = None
        self.clock = time.monotonic
        self.wall = time.time
        # Seams for the laboratory's own tests only; the command line never sets them.
        self.host_source = None
        self.lock_directory = None
        self.platform = None
        for key, value in values.items():
            require(hasattr(self, key), "unknown shard option " + key)
            setattr(self, key, value)


# The run bundle --------------------------------------------------------------------------------

def _read(path, limit=limits.MAX_JSON_BYTES):
    with open(path, "rb") as stream:
        data = stream.read(limit + 1)
    require(len(data) <= limit, "%s exceeds %d bytes" % (path, limit))
    return data


def prepare_run_dir(run_dir, plan_bytes, builds_bytes):
    """Create the run bundle (`run.json`, `builds.json`, `manifest.json`) or verify that an
    existing one holds exactly these bytes; return the shard root."""
    if not os.path.lexists(run_dir):
        os.makedirs(run_dir)
        for name, data in (("run.json", plan_bytes), ("builds.json", builds_bytes)):
            with open(os.path.join(run_dir, name), "wb") as stream:
                stream.write(data)
        bundle.write_manifest(run_dir, limits.MAX_JSON_BYTES * 2, skip=(SHARD_DIR,))
        os.mkdir(os.path.join(run_dir, SHARD_DIR))
    else:
        _, hashes = bundle.verify(run_dir, limits.MAX_JSON_BYTES * 2,
                                  allowed=lambda path: path in RUN_FILES, skip=(SHARD_DIR,),
                                  label="run")
        require(hashes.get("run.json") == sha256_bytes(plan_bytes)
                and hashes.get("builds.json") == sha256_bytes(builds_bytes),
                "%s holds another plan or build; a run directory belongs to one run" % run_dir)
        shards = os.path.join(run_dir, SHARD_DIR)
        if not os.path.lexists(shards):
            os.mkdir(shards)
        require(os.path.isdir(shards) and not os.path.islink(shards),
                "%s/shards is not a directory" % run_dir)
    return os.path.join(run_dir, SHARD_DIR)


# Authority inputs ------------------------------------------------------------------------------

class Authority(object):
    """The plan's measuring authority, read from its Git objects: the registry the plan froze,
    its golden entries (checked against this driver's reference generator), its CLI registry
    and the CLI fixture files, materialized read-only below `work`."""

    def __init__(self, git, plan, work):
        sha = plan["resolved"]["authority_sha"]
        suite = planning.read_suite(git, sha)
        require(suite.policy_digest == plan["authority"]["policy_digest"]
                and suite.raw_sha256 == plan["authority"]["suite_sha256"],
                "the authority %s registry is not the one the plan froze" % sha[:12])
        self.doc = suite.doc
        golden_path = "fixtures/performance/" + self.doc["probe"]["golden"]
        raw = git.read_file(sha, golden_path)
        require(raw is not None, "the authority has no %s" % golden_path)
        self.golden = contracts.read_json(raw, "golden", golden_path)
        oracle.check_coverage(self.doc, self.golden)
        oracle.check_reference(self.doc, self.golden)
        self.cli = None
        self.fixtures = None
        if any(case["kind"] == "process" for case in plan["cases"]):
            raw = git.read_file(sha, clicases.CLI_PATH)
            if raw is None:
                raise Unavailable("no_cli_registry", "the authority %s has no %s; its process "
                                  "cases have no CLI oracle" % (sha[:12], clicases.CLI_PATH))
            self.cli = clicases.check_registry(clicases.load_bytes(raw), self.doc)
        for case in plan["cases"]:
            if case["seed"] is not None:
                # The plan's seed is the authority registry's, which the plan froze.
                require(oracle.seed_of(self.doc, case["instance_id"]) == case["seed"],
                        "%s: the plan's seed is not the authority registry's" % case["instance_id"])
        if self.cli is not None or any(case["seed"] is not None for case in plan["cases"]):
            # The authority's fixture setup (setup.sh) copies seeds and prepares CLI fixtures.
            self.fixtures = os.path.join(work, "fixtures")
            _materialize(git, sha, "fixtures/performance/cli", self.fixtures)


def _materialize(git, sha, prefix, target):
    """Write every file below `prefix` at `sha` to `target`, read-only."""
    entries = git.tree_entries(sha, [prefix])
    blobs = git.blobs([obj for _, kind, obj, _ in entries if kind == "blob"])
    os.makedirs(target, 0o700)
    for mode, kind, obj, path in entries:
        require(kind == "blob" and mode in ("100644", "100755"),
                "%s at %s is not a regular file" % (path, sha[:12]))
        relative = path[len(prefix) + 1:]
        destination = os.path.join(target, relative)
        os.makedirs(os.path.dirname(destination), 0o700, exist_ok=True)
        with open(destination, "wb") as stream:
            stream.write(blobs[obj])
        os.chmod(destination, 0o500 if mode == "100755" else 0o400)
    for current, dirs, _ in os.walk(target):
        for name in dirs:
            os.chmod(os.path.join(current, name), 0o500)
    os.chmod(target, 0o500)


# Binaries --------------------------------------------------------------------------------------

def read_build_manifest(build_dir, plan):
    """The build's `builds.json` (document, bytes), read within the JSON bound and validated,
    of this very run and plan; no artifact is read (ADR-0172)."""
    manifest = builds.read_manifest(os.path.abspath(build_dir))
    require(manifest[0]["run_id"] == plan["run_id"]
            and manifest[0]["plan_digest"] == plan["plan_digest"],
            "the build at %s is of another run or plan" % build_dir)
    return manifest


class Binaries(object):
    """The verified build's supervisor and each side's AF binary and probe, by path, hash and
    size. Every artifact is rehashed within `deadline`, the dominant remaining budget."""

    def __init__(self, build_dir, manifest, deadline):
        build_dir = os.path.abspath(build_dir)
        self.doc, paths = builds.verify_artifacts(build_dir, deadline, manifest)
        if self.doc["supervisor"]["status"] != "succeeded":
            raise Unavailable("build_incompatible", "the build has no authority supervisor")
        record = self.doc["supervisor"]["artifact"]
        self.supervisor = (paths["supervisor"], record["sha256"], record["size"])
        self.paths = {}
        for side in contracts.SIDES:
            artifacts = self.doc["sides"][side]["artifacts"]
            for kind, key in (("process", "af"), ("library", "probe")):
                if artifacts[key] is not None:
                    self.paths[(side, kind)] = (paths[side + "/" + key],
                                                artifacts[key]["sha256"], artifacts[key]["size"])
        self.builds_bytes = manifest[1]

    def get(self, side, kind):
        """(path, sha256, size) of a side's binary of a case kind, or None when the build has
        none."""
        return self.paths.get((side, kind))

    def verified(self, entry, deadline):
        """The binary's path after rehashing it within `deadline`; a deadline that ends the
        rehash is the deadline's (`budget.Expired`), anything else a changed artifact."""
        path, digest, size = entry
        try:
            builds.verified_binary(path, digest, deadline, size)
        except budget.Expired:
            raise
        except (ContractError, OSError) as error:
            raise Stop("binary_changed", "a verified artifact changed during the shard: %s"
                       % error)
        return path


# The shard -------------------------------------------------------------------------------------

def _host_cpus():
    """(online CPU IDs, {cpu: siblings}) of this Linux host, from sysfs."""
    with open("/sys/devices/system/cpu/online") as stream:
        online = _expand_list(stream.read().strip())
    siblings = {}
    for cpu in online:
        path = "/sys/devices/system/cpu/cpu%d/topology/thread_siblings_list" % cpu
        try:
            with open(path) as stream:
                siblings[cpu] = set(_expand_list(stream.read().strip()))
        except OSError:
            siblings[cpu] = {cpu}
    return online, siblings


def _expand_list(text):
    cpus = []
    for part in text.split(","):
        if "-" in part:
            first, last = part.split("-", 1)
            cpus.extend(range(int(first), int(last) + 1))
        elif part:
            cpus.append(int(part))
    return cpus


def contained_host(docker, image, scratch_fs, environment, run_budget, clock, before_probe=None):
    """(scratch root, CPUs, runtime identity) of a host that can run samples in `environment`,
    the contained Linux profile: the image pinned by digest, a Linux container runtime with
    cgroup v2, the dedicated scratch filesystem's bound verified by an enforcement fill, and the
    environment's logical CPUs. Every step is bounded by `run_budget`; a host that cannot is
    refused with its reason (`Unavailable`), never given a weaker substitute. Shards and
    contained diagnostic child runs share it (ADR-0176)."""
    require(image and (image.startswith("sha256:") or "@sha256:" in image),
            "--image: the sample image pinned by digest")
    require(scratch_fs, "--scratch-fs: the dedicated disk-bounded scratch "
            "filesystem's mount point")
    query = "querying the container runtime"
    seconds = run_budget.seconds(query, capability.TOOL_TIMEOUT_SECONDS)
    info = capability.docker_info(docker, timeout=seconds)
    if info["unavailable"]:
        if seconds < capability.TOOL_TIMEOUT_SECONDS and run_budget.left() <= 0:
            raise run_budget.expired(query, seconds)
        raise Unavailable("no_container_runtime", info["unavailable"])
    fields = info["identity"].split(" ")
    if fields[0] != "linux" or (len(fields) > 2 and fields[2] != "cgroup2"):
        raise Unavailable("container_runtime_unsupported",
                          "the container runtime is %s, not Linux with cgroup v2"
                          % info["identity"])
    limit = environment["scratch_limit_bytes"]
    # The check keeps every enforcement and error check; only its time bound is the run's
    # remaining time when that is shorter, fixed as one instant before its first step
    # (ADR-0173), and a check the deadline ended is the deadline's.
    fill = "verifying the scratch filesystem's bound"
    started = clock()
    deadline = run_budget.until(fill, capability.FILL_SECONDS_MAX, started)
    if before_probe is not None:
        before_probe()
    record = capability.scratch_capability(scratch_fs, limit, verify_enforcement=True,
                                           host=capability.Host(clock=clock),
                                           deadline=deadline)
    cleanup_confirmed = (record["fill"] or {}).get("cleanup") is None
    if record["capability"]["status"] != "available":
        if capability.ended_by_deadline(record) \
                and deadline - started < capability.FILL_SECONDS_MAX:
            expired = run_budget.expired(fill, deadline - started)
            expired.cleanup_confirmed = cleanup_confirmed
            raise expired
        raise Unavailable("scratch_unverified", "no verified disk-bounded scratch: "
                          + "; ".join(record["capability"]["reasons"]),
                          cleanup_confirmed=cleanup_confirmed)
    from . import builds
    try:
        builds.refuse_implicit_volumes(docker, image, run_budget)
    except builds.Unavailable as error:
        raise Unavailable(error.reason, str(error))
    run_budget.seconds("reading the host's CPUs")
    online, siblings = _host_cpus()
    cpus, shared = supervise.choose_cpus(online, siblings, environment["logical_cpus"])
    runtime = "%s; image %s; cpus %s%s" % (
        info["identity"], image, ",".join(str(cpu) for cpu in cpus),
        " sharing a physical core" if shared else "")
    return record["path"], cpus, runtime


class Shard(object):
    def __init__(self, options, lock_path):
        self.options = options
        self.lock_path = lock_path
        plan_bytes = _read(options.plan)
        self.plan = contracts.read_json(plan_bytes, "run", options.plan)
        plan = self.plan
        require(plan["scope"] != "diagnostic", "a diagnostic plan is a profile run, not a shard")
        require(isinstance(options.shard, int)
                and 0 <= options.shard < len(plan["shards"]),
                "--shard: one of 0..%d" % (len(plan["shards"]) - 1))
        self.index = options.shard
        self.slots = schedule.expected_slots(plan, self.index)
        self.cases = dict((case["instance_id"], case) for case in plan["cases"])
        # The build manifest binds the run's deadline before any artifact is hashed, and every
        # preflight step, the artifacts' admission first, ends FINISH_RESERVE_SECONDS before
        # it (ADR-0172).
        self.manifest = read_build_manifest(options.build, plan)
        self.run_execution = self.check_run_deadline()
        self.budget = budget.Budget(self.run_deadline, options.clock, FINISH_RESERVE_SECONDS)
        # Until the samples start, the preflight's budget bounds every rehash and seed read.
        self.sampling_budget = self.budget
        self.bound = None
        try:
            self.budget.seconds("verifying the build's artifacts")
            self.binaries = Binaries(options.build, self.manifest, self.budget)
        except budget.Expired as expired:
            raise Unavailable("run_deadline", str(expired),
                              cleanup_confirmed=getattr(expired, "cleanup_confirmed", True))
        self.seed_bounds = seeds.bounds(plan["profile"]["environment"]["scratch_limit_bytes"])
        self.check_states()
        self.mode = self.check_mode()
        root = os.path.join(options.run_dir, SHARD_DIR, str(self.index))
        require(not os.path.lexists(root), "%s already exists; a shard runs once, and a rerun "
                "is a new run" % root)
        self.git = gitrefs.Git(os.path.abspath(options.repo), budget=self.budget)
        self.work = tempfile.mkdtemp(prefix="af-perf-shard-")
        try:
            self.budget.seconds("reading the plan's authority")
            self.authority = Authority(self.git, plan, self.work)
            self.versions = {}
            self.adapters = {}
            self.receipts = {}
            if self.authority.cli is not None:
                self.versions = clicases.side_versions(self.git, plan)
                self.receipts = clicases.side_receipts(self.git, plan)
                for side, version in self.versions.items():
                    self.adapters[side] = clicases.choose_adapter(self.authority.cli, version)
            self.cpus = None
            self.container_runtime = None
            if self.mode == LINUX_CONTAINER:
                self.prepare_container()
            self.budget.seconds("reading the host's identity")
            self.identity = host.identity(self.container_runtime, options.host_source)
            self.host_id = contracts.host_id(self.identity)
            self.injection = self.injection_group()
            self.budget.seconds("creating the run directory")
            # Only a host that can run the shard creates its run directory.
            self.root = os.path.join(prepare_run_dir(options.run_dir, plan_bytes,
                                                     self.binaries.builds_bytes),
                                     str(self.index))
            require(not os.path.lexists(self.root), "%s already exists; a shard runs once, "
                    "and a rerun is a new run" % self.root)
        except budget.Expired as expired:
            _remove_work(self.work)
            raise Unavailable("run_deadline", str(expired),
                              cleanup_confirmed=getattr(expired, "cleanup_confirmed", True))
        except BaseException:
            _remove_work(self.work)
            raise
        self.log_budget = plan["limits"]["shard_bundle_bytes"] // 2
        self.logs_bytes = 0
        self.records = 0
        self.failed = 0
        self.skipped = {}
        self.interrupted = None
        self.seeds = {}
        self.seed_store = None
        self.seed_namespace = None
        self.seed_cleanup = []
        self.deadline = None
        self.deadline_unix_ms = None
        # Scratch kept for recovery because a process that could reach it may still be live.
        self.retained = []

    # Preconditions ------------------------------------------------------------------------

    def injection_group(self):
        """A preparation or slowdown member's loop counts for the CPU group this host belongs
        to (ADR-0181); None for any other plan. A host of a group the plan did not prepare
        refuses the shard before any sample: its injection could not be the declared dose."""
        cohort = self.plan["cohort"]
        if cohort is None or cohort["injection"] is None:
            return None
        from . import calibration
        model = self.identity["cpu_model"].get("value")
        image = self.options.image if self.mode == LINUX_CONTAINER else None
        group = calibration.host_group(cohort["injection"], model, image)
        if group is None:
            raise Unavailable("unprepared_cpu_group", "this host's CPU %r and sample image %r "
                              "are no CPU group member %s was prepared for; nothing is "
                              "sampled" % (model, image, cohort["member_id"]))
        return group

    def injection_environment(self, slot):
        """The loop control of one sample: the declared count for a candidate's warmup or
        measured sample of a preparation or slowdown member, 0 for its baseline and sentinels,
        and nothing at all for any other plan."""
        if self.injection is None:
            return {}
        count = 0
        if slot["side"] == schedule.CANDIDATE and slot["phase"] in ("warmup", "measured"):
            count = self.injection["iterations"][slot["instance_id"]]
        return {INJECTION_ENV: str(count)}

    def record_injection(self, slot, record, supervised, extra):
        """Keep the measured injection the process reported for its declared count."""
        if self.injection is None or slot["instance_id"] == self.plan["health"]["workload"]:
            return
        from . import calibration
        count = int(self.injection_environment(slot)[INJECTION_ENV])
        metric, why = calibration.injected_report(supervised.stderr, count)
        if metric is not None:
            extra[calibration.INJECTED_METRIC] = (metric, "probe_reported")
        else:
            record["unavailable_metrics"][calibration.INJECTED_METRIC] = why[:256]

    def check_run_deadline(self):
        """The run's execution start and total deadline from its verified build (ADR-0170): a
        shard never resets them, and none starts its preflight once the deadline has passed."""
        run = self.manifest[0]["execution"]["run"]
        require(run["total_minutes"] == self.plan["limits"]["run_total_minutes"],
                "the build's run deadline is not the plan's frozen %d minutes"
                % self.plan["limits"]["run_total_minutes"])
        now = int(self.options.wall() * 1000)
        if now < run["started_unix_ms"]:
            raise Unavailable("clock_skew", "this host's clock reads %d ms, before the run's "
                              "execution start %d ms that its build recorded"
                              % (now, run["started_unix_ms"]))
        if now >= run["deadline_unix_ms"]:
            raise Unavailable("run_deadline", "the run's total deadline passed %d ms ago "
                              "(started %d, %d minutes); nothing was started"
                              % (now - run["deadline_unix_ms"], run["started_unix_ms"],
                                 run["total_minutes"]))
        caller = self.options.deadline_unix_ms
        if caller is not None and now >= caller:
            raise Unavailable("run_deadline", "the caller's deadline %d has passed; nothing "
                              "was started" % caller)
        # The dominant deadline as an instant of the shard's monotonic clock, read once with
        # the wall clock; nothing later resets it (ADR-0171).
        self.run_deadline = self.options.clock() + (
            contracts.effective_deadline(run["deadline_unix_ms"], caller) - now) / 1000.0
        return run

    def check_states(self):
        """The plan's frozen capability states: nothing it calls not executable runs."""
        profile = self.plan["profile"]
        pending = []
        if profile["environment"]["state"] != "supported":
            pending.append("environment %s: %s" % (profile["environment_id"],
                                                   profile["environment"]["pending"]))
        if profile["recipe"]["state"] != "supported":
            pending.append("recipe %s: %s" % (profile["recipe_id"], profile["recipe"]["pending"]))
        for ident in self.plan["shards"][self.index]["instances"]:
            case = self.cases[ident]
            if case["execution"]["state"] != "supported":
                pending.append("%s: %s" % (ident, case["execution"]["pending"]))
        if pending:
            raise Unavailable("not_executable", "the plan's authority declares %s not yet "
                              "executable: %s" % (len(pending), "; ".join(pending)[:3500]))

    def check_mode(self):
        """The one execution mode the plan's environment allows on this host, or refusal."""
        environment = self.plan["profile"]["environment"]
        system = capability.host_platform()
        machine = platform.machine().lower()
        arch = {"arm64": "aarch64", "amd64": "x86_64"}.get(machine, machine)
        if self.options.platform is not None:
            system, arch = self.options.platform
        mode = self.options.mode
        wanted = (TRUSTED_LOCAL if environment["containment"] == "trusted_local_process_group"
                  else LINUX_CONTAINER)
        if mode != wanted:
            raise Unavailable("mode_mismatch", "environment %s runs as %s; --mode %s is not a "
                              "substitute" % (self.plan["profile"]["environment_id"], wanted,
                                              mode))
        if environment["os"] != system or environment["arch"] != arch:
            raise Unavailable("host_mismatch", "environment %s is %s %s; this host is %s %s"
                              % (self.plan["profile"]["environment_id"], environment["os"],
                                 environment["arch"], system, arch))
        if mode == TRUSTED_LOCAL:
            require(self.options.scratch, "trusted local mode needs --scratch, a private "
                    "parent directory owned by you")
            for ident in self.plan["shards"][self.index]["instances"]:
                require(not self.cases[ident]["requires_linux_containment"],
                        "%s spawns processes and requires Linux containment" % ident)
        return mode

    def prepare_container(self):
        options = self.options
        self.scratch_root, self.cpus, self.container_runtime = contained_host(
            options.docker, options.image, options.scratch_fs,
            self.plan["profile"]["environment"], self.budget, options.clock)

    # Execution ----------------------------------------------------------------------------

    def run(self):
        """Run every slot in order; write the terminal shard record and the bundle manifest."""
        handlers = {}

        def interrupt(signum, frame):
            self.interrupted = signum

        for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            handlers[signum] = signal.signal(signum, interrupt)
        os.makedirs(os.path.join(self.root, LOGS))
        started_unix_ms = int(self.options.wall() * 1000)
        started = self.options.clock()
        sampling_unix_ms = started_unix_ms + self.plan["limits"]["shard_sampling_minutes"] * 60000
        run_unix_ms = contracts.effective_deadline(self.run_execution["deadline_unix_ms"],
                                                   self.options.deadline_unix_ms)
        self.deadline_unix_ms = min(run_unix_ms, sampling_unix_ms)
        # The sampling deadline starts with the samples; the run's was fixed before the
        # preflight and is never reset (ADR-0171).
        sampling = started + self.plan["limits"]["shard_sampling_minutes"] * 60
        deadline = min(sampling, self.run_deadline)
        bound = "sampling_deadline" if sampling <= self.run_deadline else "run_deadline"
        self.deadline = deadline
        self.bound = bound
        # Every rehash and seed read of a sample leaves the terminal reserve (ADR-0172).
        self.sampling_budget = budget.Budget(deadline, self.options.clock, FINISH_RESERVE_SECONDS)
        observations = {"start": host.load(self.options.host_source), "end": None}
        stop = None
        try:
            with open(os.path.join(self.root, SAMPLES), "ab") as samples:
                for slot in self.slots:
                    if self.interrupted is not None:
                        stop = Stop("cancelled", "signal %d ended the shard before sequence %d"
                                    % (self.interrupted, slot["sequence"]), "interrupted")
                        break
                    worst = self.worst_case_seconds(slot)
                    if self.options.clock() + worst + FINISH_RESERVE_SECONDS > deadline:
                        stop = Stop(bound, "sequence %d's worst case of %d s would pass the "
                                    "shard's deadline (%s); %d slots unstarted"
                                    % (slot["sequence"], worst, bound.replace("_", " "),
                                       len(self.slots) - slot["sequence"]), "interrupted")
                        break
                    why = self.unavailable(slot)
                    if why is not None:
                        self.skipped.setdefault(slot["instance_id"], why)
                        continue
                    try:
                        record = self.sample(slot)
                    except Stop as halted:
                        record, stop = halted.record, halted
                    except Exception as error:  # noqa: BLE001 - sealed, never swallowed
                        # The driver itself failed: what was recorded stays, sealed under a
                        # terminal record that says why the rest never ran.
                        record, stop = None, Stop("driver_error", "sequence %d: %s: %s"
                                                  % (slot["sequence"], type(error).__name__,
                                                     error))
                    if record is not None:
                        samples.write(document_bytes(contracts.validate_sample(record)))
                        samples.flush()
                        self.records += 1
                        self.failed += record["status"] == "failed"
                    if stop is not None:
                        break
        finally:
            for signum, handler in handlers.items():
                signal.signal(signum, handler)
            if stop is not None and stop.retain:
                # A process that may still be live could read or write the shard's seeds.
                if self.seed_namespace is not None:
                    self.retained.append(self.seed_namespace.keep())
                elif self.seed_store is not None:
                    self.retained.append(self.seed_store)
            else:
                self.remove_seeds()
            _remove_work(self.work)
        observations["end"] = host.load(self.options.host_source)
        return self.finish(stop, started_unix_ms, observations)

    def worst_case_seconds(self, slot):
        if slot["instance_id"] == self.plan["health"]["workload"]:
            timeout_ms = oracle.timeout_for(self.authority.doc, slot["instance_id"]) * 1000
            setup_ms = 0
        else:
            case = self.cases[slot["instance_id"]]
            timeout_ms = case["sample_timeout_ms"]
            # A CLI sample's setup, or a seeded library sample's private copy of its seed.
            setup_ms = SETUP_TIMEOUT_MS if case["kind"] == "process" or case["seed"] else 0
            seed = self.pending_seed(slot, case)
            if seed is not None:
                # The side's seed is written before its first sample: a CLI seed's supervised
                # setup and its publication, or a native seed's writer, each with its own
                # deadline and cleanup grace.
                timeout_ms += (SETUP_TIMEOUT_MS + SEED_PUBLISH_TIMEOUT_MS + 3 * GRACE_MS
                               if seed[0] == "cli" else SEED_WRITER_TIMEOUT_MS + 2 * GRACE_MS) \
                    + SAMPLE_MARGIN_SECONDS[self.mode] * 1000
        return (timeout_ms + setup_ms + 3 * GRACE_MS) / 1000.0 + SAMPLE_MARGIN_SECONDS[self.mode]

    def pending_seed(self, slot, case):
        """(kind, side, seed ID) of the seed a slot copies that its side has not written yet,
        or None."""
        if case["kind"] == "library":
            if case["seed"] is None:
                return None
            key = (slot["side"], case["seed"]["seed_id"])
            return None if key in self.seeds else ("native",) + key
        if slot["side"] not in self.adapters:
            return None
        adapter = self.adapters[slot["side"]][1]
        if not isinstance(adapter, dict):
            return None
        entry = clicases.command(adapter, case["case_id"], case["parameters"])
        if entry is None or entry.get("setup") not in ("seed_copy",
                                                       "registry_stores_20x10000"):
            return None
        key = (slot["side"], entry["seed"])
        return None if key in self.seeds else ("cli",) + key

    def finish(self, stop, started_unix_ms, observations):
        with open(os.path.join(self.root, SAMPLES), "rb") as stream:
            data = stream.read()
        status = "complete" if stop is None else stop.status
        doc = {
            "schema": contracts.SHARD_SCHEMA, "run_id": self.plan["run_id"],
            "plan_digest": self.plan["plan_digest"], "shard": self.index, "status": status,
            "provenance": "measured", "host_id": self.host_id, "host": self.identity,
            "execution": {
                "mode": self.mode,
                "containment": "pid_namespace" if self.mode == LINUX_CONTAINER
                else "process_group",
                "sample_image": self.options.image if self.mode == LINUX_CONTAINER else None,
                "supervisor_sha256": self.binaries.supervisor[1],
                "builds_sha256": sha256_bytes(self.binaries.builds_bytes),
                "sampling_deadline_ms": self.plan["limits"]["shard_sampling_minutes"] * 60000,
                "run": self.run_execution,
                "caller_deadline_unix_ms": self.options.deadline_unix_ms,
                "deadline_unix_ms": self.deadline_unix_ms,
                "sample_cpus": self.cpus, "coordination_lock": self.lock_path},
            "seeds": [self.seeds[key].record for key in sorted(self.seeds)],
            "observations": observations,
            "expected_records": len(self.slots), "record_count": self.records,
            "slots": {"failed": self.failed, "unstarted": len(self.slots) - self.records},
            "samples_sha256": sha256_bytes(data), "samples_bytes": len(data),
            "logs_bytes": self.logs_bytes,
            "started_unix_ms": started_unix_ms,
            "ended_unix_ms": max(started_unix_ms, int(self.options.wall() * 1000)),
            "failure": None if stop is None else {"reason": stop.reason, "detail": stop.detail},
        }
        if stop is not None and self.retained:
            doc["failure"]["detail"] = ("%s; kept for recovery: %s" % (
                stop.detail, ", ".join(self.retained)))[:4096]
        if stop is None and self.seed_cleanup:
            # A seed left behind is shared state a later shard would find.
            doc["status"] = "failed"
            doc["failure"] = {"reason": "seed_cleanup_failed", "detail": (
                "the shard's seeds were not removed: " + "; ".join(self.seed_cleanup))[:4096]}
        elif stop is None and self.records != len(self.slots):
            # Every runnable slot ran; the slots of instances this host or build cannot cover
            # stay unrecorded, so the shard is not complete evidence of its plan.
            doc["status"] = "failed"
            doc["failure"] = {"reason": "unavailable_coverage", "detail": (
                "%d of %d slots have no record: %s" % (
                    len(self.slots) - self.records, len(self.slots),
                    "; ".join("%s: %s" % item for item in sorted(self.skipped.items()))))[:4096]}
        with open(os.path.join(self.root, SHARD_FILE), "wb") as stream:
            stream.write(contracts.write_bytes(doc, "shard"))
        bundle.write_manifest(self.root, self.plan["limits"]["shard_bundle_bytes"])
        return doc

    # One sample -----------------------------------------------------------------------------

    def _case(self, slot):
        if slot["instance_id"] == self.plan["health"]["workload"]:
            return None, "library"
        case = self.cases[slot["instance_id"]]
        return case, case["kind"]

    def unavailable(self, slot):
        """Why this host or build cannot run a slot's instance at all, or None. Such a slot is
        never substituted: it stays unrecorded and the shard incomplete."""
        side = slot["side"]
        case, kind = self._case(slot)
        if self.binaries.get(side, kind) is None:
            return "the %s build has no %s binary (%s)" % (
                side, "AF" if kind == "process" else "probe",
                self.binaries.doc["sides"][side]["coverage_reason"] or "build failed")
        if kind != "process":
            return None
        adapter_id, adapter = self.adapters[side]
        if adapter_id is None:
            return "the %s side has no CLI coverage: %s" % (side, adapter)
        entry = clicases.command(adapter, case["case_id"], case["parameters"])
        if entry is None or "pending" in entry:
            return "CLI adapter %s has no runnable command: %s" % (
                adapter_id, entry["pending"] if entry else "not declared")
        if entry["setup"] in ("seed_copy", "registry_stores_20x10000") and \
                self.binaries.get(side, "library") is None:
            return "the %s build has no probe to write the CLI seed %s" % (side, entry["seed"])
        return None

    def sample(self, slot):
        """Run one available slot and return its record; a Stop carries its record, if any."""
        case, kind = self._case(slot)
        binary = self.binaries.get(slot["side"], kind)
        if kind == "library":
            return self.library_sample(slot, case, binary)
        return self.process_sample(slot, case, binary)

    def base_record(self, slot, case, binary_sha, started_unix_ms):
        workload = self.plan["health"]["workload"]
        side = slot["side"]
        sentinel = case is None
        source = self.plan["resolved"]["baseline_sha" if side == schedule.BASELINE
                                       else "candidate_sha"]
        return {
            "schema": contracts.SAMPLE_SCHEMA, "run_id": self.plan["run_id"],
            "plan_digest": self.plan["plan_digest"], "shard": self.index,
            "sequence": slot["sequence"], "phase": slot["phase"],
            "instance_id": slot["instance_id"], "pair": slot["pair"],
            "position": slot["position"], "side": side, "source_sha": source,
            "binary_sha256": binary_sha, "host_id": self.host_id,
            "started_unix_ms": started_unix_ms, "status": "failed",
            "identity": {"case_id": workload if sentinel else case["case_id"],
                         "contract_id": None, "adapter": None,
                         "parameters": {} if sentinel else dict(case["parameters"]),
                         "fixture_digest": None},
            "metrics": {}, "metric_scopes": {}, "unavailable_metrics": {},
            "oracle": {"status": "not_run",
                       "kind": (self.plan["health"]["oracle"]["kind"] if sentinel
                                else case["oracle"]["kind"]),
                       "observed_digest": None, "detail": ""},
            "execution": {"containment": "pid_namespace" if self.mode == LINUX_CONTAINER
                          else "process_group", "termination": None, "exit_code": None,
                          "signal": None, "exit_observation": None, "lifecycle": None,
                          "host_wall_ns": None, "fixture_setup_ns": None},
            "failure": None,
            "logs": {"stdout_bytes": 0, "stderr_bytes": 0, "stdout_sha256": sha256_bytes(b""),
                     "stderr_sha256": sha256_bytes(b""), "truncated_bytes": 0,
                     "retained": None},
        }

    def declared_optional(self, case):
        if case is None:
            return []
        keys = [metric["key"] for metric in case["optional_metrics"]]
        if self.injection is not None:
            from . import calibration
            keys.append(calibration.INJECTED_METRIC)
        return keys

    def supervise(self, order, side, kind_binaries, readonly=None, seed_output=None):
        """Launch one supervised sample in this shard's mode; (supervised, stop or None)."""
        supervisor = self.verified(self.binaries.supervisor)
        if self.mode == TRUSTED_LOCAL:
            try:
                return supervise.launch([supervisor], order, deadline=self.deadline,
                                        clock=self.options.clock), None
            except supervise.SupervisorFailure as error:
                raise self.supervision_stop(error, "the supervisor")
        name = "af-perf-%s-%d-%s" % (self.plan["run_id"][-12:], self.index,
                                     os.urandom(4).hex())
        try:
            contained = supervise.run_contained(
                self.options.docker, name, self.options.image, self.plan["profile"]["environment"],
                supervisor, kind_binaries, self.scratch_root, self.options.uid,
                self.options.gid, self.cpus, order, readonly=readonly, seed_output=seed_output,
                deadline=self.deadline, clock=self.options.clock)
        except supervise.SupervisorFailure as error:
            raise self.supervision_stop(error, "the sample container")
        problems = []
        if not contained.removed:
            problems.append(("container_removal_unconfirmed", contained.removal))
        if contained.scratch_problems:
            problems.append(("scratch_not_restored", "; ".join(contained.scratch_problems)))
        if contained.limit_problems:
            problems.append(("environment_unverified", "; ".join(contained.limit_problems)))
        # A container whose removal is not confirmed may still run with the shard's seed bound.
        stop = Stop(problems[0][0], "; ".join(detail for _, detail in problems),
                    retain=not contained.removed) if problems else None
        return contained.supervised, stop

    def verified(self, entry):
        """A binary rehashed within the dominant deadline immediately before its launch."""
        try:
            return self.binaries.verified(entry, self.sampling_budget)
        except budget.Expired as expired:
            raise self.deadline_stop(expired)

    def deadline_stop(self, expired):
        """The stop of work the dominant deadline ended: it interrupts the shard."""
        return Stop(self.bound or "run_deadline", str(expired), "interrupted")

    def supervision_stop(self, error, what):
        """The stop a supervisor without a judgeable reply demands (ADR-0171): an unconfirmed
        end of its sample keeps everything the sample could reach; the run's deadline, having
        refused the launch or cancelled the sample with its end confirmed, interrupts the
        shard; anything else fails it."""
        if error.uncertain:
            return Stop("cleanup_uncertain", "%s gave no confirmation that its sample ended, so "
                        "the shard stops and keeps what the sample could reach: %s"
                        % (what, error), retain=True)
        if error.deadline:
            return Stop(self.bound or "run_deadline", "%s: %s" % (what, error), "interrupted")
        return Stop("supervisor_failed", "%s gave no valid reply: %s" % (what, error))

    def apply_supervised(self, record, supervised):
        result = supervised.result
        termination = result["termination"]
        record["execution"].update(
            termination=termination["kind"], exit_code=termination["code"],
            signal=termination["signal"],
            exit_observation=result["timing"]["exit_observation"],
            lifecycle=result["lifecycle"]["status"],
            host_wall_ns=str(supervised.host_wall_ns),
            fixture_setup_ns=(result["setup"]["wall_ns"] if result["setup"] is not None
                              else None))
        stdout, stderr = supervised.stdout, supervised.stderr
        record["logs"].update(stdout_bytes=len(stdout), stderr_bytes=len(stderr),
                              stdout_sha256=sha256_bytes(stdout),
                              stderr_sha256=sha256_bytes(stderr),
                              truncated_bytes=result["stdout"]["truncated"]
                              + result["stderr"]["truncated"])

    def optional(self, record, case, result, extra=None):
        """Every declared optional metric: reported with its scope, or unavailable with why."""
        wanted = self.declared_optional(case)
        metrics, unavailable = supervise.optional_metrics(result, wanted) if result else ({}, {})
        for key, metric in metrics.items():
            record["metrics"][key] = metric
            record["metric_scopes"][key] = ("sample_cgroup" if key == "cgroup_peak_memory_bytes"
                                            else "reaped_child_rusage")
        for key, (metric, scope) in (extra or {}).items():
            if key in wanted:
                record["metrics"][key] = metric
                record["metric_scopes"][key] = scope
        for key in wanted:
            if key not in record["metrics"]:
                record["unavailable_metrics"].setdefault(key, unavailable.get(
                    key, "not observed by this sample")[:256])

    def fail(self, record, kind, detail, supervised=None):
        record["status"] = "failed"
        record["failure"] = {"kind": kind, "detail": oracle.bounded(detail, 4096)}
        if supervised is not None:
            self.retain(record, supervised)
        return record

    def retain(self, record, supervised):
        """Keep a failed sample's capped output in the bundle while the log budget lasts."""
        data = supervised.stdout + supervised.stderr
        if not data or self.logs_bytes + len(data) > self.log_budget:
            return
        relative = "%s/%06d.log" % (LOGS, record["sequence"])
        with open(os.path.join(self.root, relative), "wb") as stream:
            stream.write(data)
        self.logs_bytes += len(data)
        record["logs"]["retained"] = relative

    def library_sample(self, slot, case, binary):
        instance_id = slot["instance_id"]
        timeout_ms = (case["sample_timeout_ms"] if case is not None
                      else oracle.timeout_for(self.authority.doc, instance_id) * 1000)
        side = slot["side"]
        seed = None
        if case is not None and case["seed"] is not None:
            # Written once per side, before the first sample that copies it (ADR-0170).
            seed = self.ensure_native_seed(side, case)
        started_unix_ms = int(self.options.wall() * 1000)
        build_side = self.binaries.doc["sides"][side]
        record = self.base_record(slot, case, binary[1], started_unix_ms)
        record["identity"]["adapter"] = build_side["chosen_adapter"]
        if seed is not None:
            if seed.digest is None:
                # The side's own writer failed: the side's failure, never another fixture.
                self.fail(record, "launch_failure", "the %s side's seed %s was not written, so "
                          "nothing was measured: %s" % (side, seed.seed_id,
                                                        seed.record["failure"]["detail"]))
                self.optional(record, case, None)
                return record
            self.check_seed(seed, "before sequence %d" % slot["sequence"])
            record["identity"]["fixture_digest"] = seed.digest
        namespace = None
        if self.mode == TRUSTED_LOCAL:
            namespace = oracle.Namespace(self.options.scratch)
            scratch, containment = namespace.path, "process_group"
        else:
            scratch, containment = supervise.SCRATCH, "pid_namespace"
        try:
            setup, readonly, probe_scratch, seed_copy = None, None, scratch, None
            if seed is not None:
                # The sample's untimed setup copies the sealed seed into its own scratch; the
                # probe works in `probe/` and reads only its copy (`seed/`).
                setup_script, source = os.path.join(self.authority.fixtures, "setup.sh"), \
                    seed.path
                if self.mode == LINUX_CONTAINER:
                    setup_script = supervise.FIXTURES + "/setup.sh"
                    readonly = {supervise.FIXTURES: self.authority.fixtures,
                                supervise.SEED_INPUT: seed.path}
                    source = supervise.SEED_INPUT
                probe_scratch = scratch + "/" + NATIVE_PROBE_SCRATCH
                seed_copy = scratch + "/" + NATIVE_SEED_COPY
                setup = (["/bin/sh", setup_script, "native_seed_copy", scratch, "-", "-",
                          source], supervise.probe_environment(scratch), scratch,
                         SETUP_TIMEOUT_MS)
            request = oracle.request_for(self.authority.doc, instance_id, probe_scratch,
                                         seed_copy)
            record["identity"]["contract_id"] = request["contract_id"]
            path = self.verified(binary)
            program = path if self.mode == TRUSTED_LOCAL else CONTAINER_BINARIES["probe"]
            environment = dict(supervise.probe_environment(scratch),
                               **self.injection_environment(slot))
            order = supervise.request(
                [program], environment, scratch,
                canonical_bytes(request).decode("utf-8"), timeout_ms, limits.RECORD_BYTES_MAX,
                self.plan["limits"]["sample_log_bytes"], containment=containment,
                scratch=None if self.mode == TRUSTED_LOCAL else supervise.SCRATCH,
                grace_ms=GRACE_MS, setup=setup)
            supervised, stop = self.supervise(order, side, {"perf-probe": path}, readonly)
        except Stop as halted:
            self._release_namespace(namespace, record, halted)
            raise
        self.apply_supervised(record, supervised)
        kind, detail, halt = supervise.outcome(supervised, containment)
        if kind is not None:
            self.fail(record, kind, detail, supervised)
            self.optional(record, case, supervised.result)
        else:
            judged = oracle.judge(self.authority.doc, self.authority.golden, request,
                                  supervise.returncode(supervised.result), supervised.stdout)
            record["oracle"].update(status=judged["oracle"]["status"],
                                    observed_digest=judged["oracle"]["observed_digest"],
                                    detail=("matches the golden entry" if judged["status"]
                                            == "passed" else judged["failure"]["detail"])[:1024])
            primary = self.plan["health"]["metric"] if case is None \
                else case["primary_metric"]["key"]
            extra = {}
            self.record_injection(slot, record, supervised, extra)
            for key, metric in judged["metrics"].items():
                scope = {"wall_ms": "library_region", "setup_ms": "probe_fixture_setup"}.get(
                    key, "probe_reported")
                if key == primary:
                    record["metrics"][key] = metric
                    record["metric_scopes"][key] = scope
                else:
                    extra[key] = (metric, scope)
            self.optional(record, case, supervised.result, extra)
            if judged["status"] == "passed":
                record["status"] = "ok"
            else:
                self.fail(record, judged["failure"]["kind"], judged["failure"]["detail"],
                          supervised)
        if self.mode == TRUSTED_LOCAL and not supervise.cleanup_confirmed(supervised.result):
            # A process of the sample may still be live: its scratch is kept, never removed
            # beneath it, and the shard stops (ADR-0171). A container's confirmed removal ends
            # every process in it.
            halted = _halted(supervised, detail, retain=True)
            self._release_namespace(namespace, record, halted)
            raise halted.with_record(record)
        try:
            self._cleanup_namespace(namespace, record)
            if seed is not None:
                seed.record["samples"] += 1
                self.check_seed(seed, "after sequence %d" % slot["sequence"], record)
        except Stop as halted:
            raise halted.with_record(record)
        if halt:
            raise _halted(supervised, detail).with_record(record)
        if stop is not None:
            raise stop.with_record(record)
        return record

    def _release_namespace(self, namespace, record, halted):
        """A stopped sample's scratch: removed, unless the stop keeps it for recovery."""
        if namespace is None:
            return
        if halted.retain:
            self.retained.append(namespace.keep())
            return
        self._cleanup_namespace(namespace, record)

    def _cleanup_namespace(self, namespace, record):
        if namespace is None:
            return
        problems = namespace.remove()
        if problems:
            note = "the private scratch %s was not removed: %s" % (namespace.path,
                                                                    "; ".join(problems))
            if record["status"] == "ok":
                record["status"] = "failed"
                record["failure"] = {"kind": "lifecycle_violation",
                                     "detail": oracle.bounded("the sample passed; " + note,
                                                              4096)}
            raise Stop("cleanup_uncertain", note)

    def process_sample(self, slot, case, binary):
        side = slot["side"]
        adapter_id, adapter = self.adapters[side]
        entry = clicases.command(adapter, case["case_id"], case["parameters"])
        seed = None
        if entry["setup"] in ("seed_copy", "registry_stores_20x10000"):
            # Written once per side, before the first sample that copies it.
            seed = self.ensure_seed(side, entry["seed"], case)
        started_unix_ms = int(self.options.wall() * 1000)
        record = self.base_record(slot, case, binary[1], started_unix_ms)
        record["identity"].update(adapter=adapter_id, contract_id=None)
        if entry["setup"] in ("task_repository", "task_started"):
            record["identity"]["fixture_digest"] = clicases.fixture_digest(
                os.path.join(self.authority.fixtures, "task-roots"))
        if seed is not None:
            if seed.digest is None:
                # The side's own writer failed: the side's failure, never another fixture.
                self.fail(record, "launch_failure", "the %s side's seed %s was not written, "
                          "so nothing was measured: %s"
                          % (side, seed.seed_id, seed.record["failure"]["detail"]))
                self.optional(record, case, None)
                return record
            self.check_seed(seed, "before sequence %d" % slot["sequence"])
            record["identity"]["fixture_digest"] = seed.digest
        probe = self.binaries.get(side, "library")
        namespace = None
        try:
            af_path = self.verified(binary)
            probe_path = self.verified(probe) if probe is not None else None
            if self.mode == TRUSTED_LOCAL:
                namespace = oracle.Namespace(self.options.scratch)
                scratch = namespace.path
                values = {"af": af_path, "probe": probe_path or "", "scratch": scratch,
                          "fixtures": self.authority.fixtures,
                          "package_version": self.versions[side]}
                setup_script = os.path.join(self.authority.fixtures, "setup.sh")
                containment, readonly = "process_group", None
                seed_argument = seed.path if seed is not None else "-"
            else:
                scratch = supervise.SCRATCH
                values = dict(CONTAINER_BINARIES, scratch=scratch,
                              fixtures=supervise.FIXTURES,
                              package_version=self.versions[side])
                setup_script = supervise.FIXTURES + "/setup.sh"
                containment, readonly = "pid_namespace", {supervise.FIXTURES:
                                                          self.authority.fixtures}
                seed_argument = "-"
                if seed is not None:
                    readonly[supervise.SEED_INPUT] = seed.path
                    seed_argument = supervise.SEED_INPUT
            environment = dict(self.cli_environment(case, scratch),
                               **self.injection_environment(slot))
            argv = [clicases.expand(text, values) for text in entry["argv"]]
            cwd = clicases.expand(entry["cwd"], values)
            setup = None
            if entry["setup"] != "none":
                argument = clicases.setup_argument(entry, self.receipts, side, seed_argument)
                setup = (["/bin/sh", setup_script, entry["setup"], scratch, values["af"],
                          values["probe"] or "-", argument], environment, scratch,
                         SETUP_TIMEOUT_MS)
            log_cap = self.plan["limits"]["sample_log_bytes"]
            order = supervise.request(
                argv, environment, cwd, "", case["sample_timeout_ms"],
                max(1, log_cap - log_cap // 4), log_cap, containment=containment,
                scratch=None if self.mode == TRUSTED_LOCAL else supervise.SCRATCH,
                grace_ms=GRACE_MS, setup=setup, patterns=clicases.patterns(entry),
                selectors=clicases.selectors(entry))
            binaries = {"af": af_path}
            if probe_path is not None:
                binaries["perf-probe"] = probe_path
            supervised, stop = self.supervise(order, side, binaries, readonly)
        except Stop as halted:
            self._release_namespace(namespace, record, halted)
            raise
        self.apply_supervised(record, supervised)
        result = supervised.result
        kind, detail, halt = supervise.outcome(supervised, containment, control_record=False)
        if kind is not None:
            self.fail(record, kind, detail, supervised)
        else:
            returncode = supervise.returncode(result)
            passed, why, observed = clicases.judge(
                entry, values, returncode, supervised.stdout, result["stdout"]["truncated"],
                result["stdout_pattern_counts"], result["stdout_json"])
            interval = None
            if passed and case["timing"]["boundary"] == "reported_interval":
                try:
                    interval = clicases.tui_interval(supervised.stdout,
                                                     case["timing"]["field"])
                except ContractError as error:
                    passed, why = False, str(error)
            record["oracle"].update(status="passed" if passed else "failed",
                                    observed_digest=observed, detail=why[:1024])
            if case["timing"]["boundary"] == "reported_interval":
                if passed:
                    record["metrics"][case["primary_metric"]["key"]] = {
                        "value": format_decimal(interval), "unit": "ms"}
                    record["metric_scopes"][case["primary_metric"]["key"]] = "probe_reported"
            else:
                wall = result["timing"]["child_wall_ns"]
                record["metrics"][case["primary_metric"]["key"]] = {
                    "value": format_decimal(Fraction(int(wall), 1000000)), "unit": "ms"}
                record["metric_scopes"][case["primary_metric"]["key"]] = \
                    "child_process_lifetime"
            extra = {}
            self.record_injection(slot, record, supervised, extra)
            for key, (metric, scope) in extra.items():
                record["metrics"][key] = metric
                record["metric_scopes"][key] = scope
            if passed:
                record["status"] = "ok"
            else:
                expected = [rule["value"] for rule in entry["rules"]
                            if rule["kind"] == "exit_code"][0]
                failure = "nonzero_exit" if returncode != expected else "oracle_mismatch"
                if failure == "nonzero_exit":
                    record["oracle"]["status"] = "failed"
                self.fail(record, failure, why, supervised)
        self.optional(record, case, result)
        if self.mode == TRUSTED_LOCAL and not supervise.cleanup_confirmed(supervised.result):
            # A process of the sample may still be live: its scratch is kept, never removed
            # beneath it, and the shard stops (ADR-0171). A container's confirmed removal ends
            # every process in it.
            halted = _halted(supervised, detail, retain=True)
            self._release_namespace(namespace, record, halted)
            raise halted.with_record(record)
        try:
            self._cleanup_namespace(namespace, record)
            if seed is not None:
                seed.record["samples"] += 1
                self.check_seed(seed, "after sequence %d" % slot["sequence"], record)
        except Stop as halted:
            raise halted.with_record(record)
        if halt:
            raise _halted(supervised, detail).with_record(record)
        if stop is not None:
            raise stop.with_record(record)
        return record

    def cli_environment(self, case, scratch):
        environment = dict(CLI_ENVIRONMENT)
        environment.update(supervise.scratch_environment(scratch))
        environment.update(case["af_environment"])
        if case["timing"]["boundary"] == "reported_interval":
            environment.update(clicases.tui_home_environment(scratch))
        return environment

    # Seeds (ADR-0169) ---------------------------------------------------------------------

    def seed_directory(self, name):
        """A new, empty directory of this shard's seed store for one seed."""
        if self.seed_store is None:
            if self.mode == TRUSTED_LOCAL:
                self.seed_namespace = oracle.Namespace(self.options.scratch)
                self.seed_store = self.seed_namespace.path
            else:
                # On the verified scratch filesystem itself, so a seed is as bounded as a
                # sample; only the shard's own seed directories are ever bound from it.
                self.seed_store = os.path.join(self.scratch_root, supervise.SEED_DIRECTORY)
                os.mkdir(self.seed_store, 0o700)
        path = os.path.join(self.seed_store, name)
        os.mkdir(path, 0o700)
        if self.mode == LINUX_CONTAINER and os.geteuid() == 0:
            # As with `sample/`, a root host hands the writer's directory over explicitly.
            os.chown(path, self.options.uid, self.options.gid)
        return path

    def ensure_seed(self, side, seed_id, case):
        """The side's seed `seed_id`, written now when it is not yet: the side's own writer
        runs as the setup of one supervised request whose child publishes the written state
        to the seed's directory; then the seed is hashed and sealed. A failure of the side's
        writer is recorded on the seed and fails every sample that needs it."""
        key = (side, seed_id)
        if key in self.seeds:
            return self.seeds[key]
        spec = self.authority.cli["seeds"][seed_id]
        af_entry = self.binaries.get(side, "process")
        probe_entry = self.binaries.get(side, "library")
        seed = Seed(side, seed_id, "cli", spec["writer"], af_entry[1], probe_entry[1])
        self.seeds[key] = seed
        seed.path = self.seed_directory("%s-%s" % (side, seed_id))
        namespace = None
        try:
            af_path = self.verified(af_entry)
            probe_path = self.verified(probe_entry)
            if self.mode == TRUSTED_LOCAL:
                namespace = oracle.Namespace(self.options.scratch)
                scratch, af, probe = namespace.path, af_path, probe_path
                setup_script = os.path.join(self.authority.fixtures, "setup.sh")
                containment, readonly, output, seed_output = "process_group", None, seed.path, \
                    None
            else:
                scratch, af, probe = supervise.SCRATCH, CONTAINER_BINARIES["af"], \
                    CONTAINER_BINARIES["probe"]
                setup_script = supervise.FIXTURES + "/setup.sh"
                containment = "pid_namespace"
                readonly = {supervise.FIXTURES: self.authority.fixtures}
                output, seed_output = supervise.SEED_OUTPUT, seed.path
            environment = dict(CLI_ENVIRONMENT)
            environment.update(supervise.scratch_environment(scratch))
            environment.update(case["af_environment"])
            request = canonical_bytes(clicases.seed_request(self.authority.cli, seed_id,
                                                            scratch)).decode("utf-8")
            order = supervise.request(
                ["/bin/sh", setup_script, "seed_publish", scratch, af, probe, output],
                environment, scratch, "", SEED_PUBLISH_TIMEOUT_MS, SEED_LOG_BYTES,
                SEED_LOG_BYTES, containment=containment,
                scratch=None if self.mode == TRUSTED_LOCAL else supervise.SCRATCH,
                grace_ms=GRACE_MS,
                setup=(["/bin/sh", setup_script, spec["writer"], scratch, af, probe, request],
                       environment, scratch, SETUP_TIMEOUT_MS))
            supervised, stop = self.supervise(order, side, {"af": af_path,
                                                            "perf-probe": probe_path},
                                              readonly, seed_output=seed_output)
        except Stop as halted:
            seed.failed("driver", halted.detail)
            if halted.retain and namespace is not None:
                self.retained.append(namespace.keep())
            else:
                self._cleanup_seed_namespace(namespace, seed)
            raise
        except Exception as error:
            # The driver failed before the writer ran: the seed says so, and the shard's
            # terminal record keeps it.
            seed.failed("driver", "%s: %s" % (type(error).__name__, error))
            self._cleanup_seed_namespace(namespace, seed)
            raise
        result = supervised.result
        # The writer is this request's setup; its child only publishes what was written.
        seed.observe(result, supervised.host_wall_ns,
                     result["setup"]["wall_ns"] if result["setup"] is not None else None)
        kind, detail, halt = supervise.outcome(supervised, containment, control_record=False)
        if kind is None and supervise.returncode(result) != 0:
            kind, detail = "launch_failure", "publishing the written seed exited %s: %s" % (
                supervise.returncode(result), supervised.stderr.decode("utf-8", "replace")
                .strip()[-1024:])
        return self.finish_seed(seed, namespace, supervised, kind, detail, halt, stop,
                                "the published seed")

    def ensure_native_seed(self, side, case):
        """The side's native seed of a library case (ADR-0170), written now when it is not yet:
        the side's own probe runs the seed's writer contract as the child of one supervised
        request, writing directly into the seed's new directory; its message is judged against
        the golden seed entry, and only then is the seed hashed and sealed. A failure is
        recorded on the seed and fails every sample that needs it; nothing is retried."""
        declared = case["seed"]
        key = (side, declared["seed_id"])
        if key in self.seeds:
            return self.seeds[key]
        probe_entry = self.binaries.get(side, "library")
        seed = Seed(side, declared["seed_id"], "native", declared["contract_id"], None,
                    probe_entry[1])
        self.seeds[key] = seed
        seed.path = self.seed_directory("%s-%s" % (side, declared["seed_id"]))
        namespace = None
        try:
            probe_path = self.verified(probe_entry)
            if self.mode == TRUSTED_LOCAL:
                namespace = oracle.Namespace(self.options.scratch)
                scratch, program, output, seed_output = namespace.path, probe_path, \
                    seed.path, None
                containment = "process_group"
            else:
                scratch, program, output, seed_output = supervise.SCRATCH, \
                    CONTAINER_BINARIES["probe"], supervise.SEED_OUTPUT, seed.path
                containment = "pid_namespace"
            request = oracle.seed_request(self.authority.doc, declared["seed_id"], output)
            order = supervise.request(
                [program], supervise.probe_environment(scratch), scratch,
                canonical_bytes(request).decode("utf-8"), SEED_WRITER_TIMEOUT_MS,
                limits.RECORD_BYTES_MAX, SEED_LOG_BYTES, containment=containment,
                scratch=None if self.mode == TRUSTED_LOCAL else supervise.SCRATCH,
                grace_ms=GRACE_MS)
            supervised, stop = self.supervise(order, side, {"perf-probe": probe_path},
                                              seed_output=seed_output)
        except Stop as halted:
            seed.failed("driver", halted.detail)
            if halted.retain and namespace is not None:
                self.retained.append(namespace.keep())
            else:
                self._cleanup_seed_namespace(namespace, seed)
            raise
        except Exception as error:
            # The driver failed before the writer ran: the seed says so, and the shard's
            # terminal record keeps it.
            seed.failed("driver", "%s: %s" % (type(error).__name__, error))
            self._cleanup_seed_namespace(namespace, seed)
            raise
        result = supervised.result
        seed.observe(result, supervised.host_wall_ns, result["timing"]["child_wall_ns"])
        kind, detail, halt = supervise.outcome(supervised, containment)
        if kind is None:
            judged = oracle.judge(self.authority.doc, self.authority.golden, request,
                                  supervise.returncode(result), supervised.stdout)
            if judged["status"] != "passed":
                failure = judged["failure"]
                kind = SEED_FAILURES.get(failure["kind"], failure["kind"])
                detail = "the writer's check does not hold: " + failure["detail"]
        return self.finish_seed(seed, namespace, supervised, kind, detail, halt, stop,
                                "the written seed")

    def finish_seed(self, seed, namespace, supervised, kind, detail, halt, stop, what):
        """Admit a written seed once its writer is known to have ended, then end the writer's
        request (ADR-0172). A writer whose end is not confirmed (trusted local mode without a
        confirming reply, or a container whose removal was not confirmed) may still be live:
        nothing of its seed is read, sealed or removed, and it and the writer's scratch are kept
        with the shard stopped. Otherwise the whole published tree is enumerated before any mode
        changes; sealing and hashing are confined to it and bounded by the seed's declared
        bounds and the dominant deadline."""
        result = supervised.result
        if self.mode == TRUSTED_LOCAL and not supervise.cleanup_confirmed(result):
            seed.failed(kind or "lifecycle_violation", "the writer's end was not confirmed, so "
                        "its seed was not admitted: " + (detail or "no detail"))
            self.retained.append(namespace.keep())
            raise _halted(supervised, detail, retain=True)
        if stop is not None and stop.retain:
            seed.failed("lifecycle_violation", "the writer's container removal was not "
                        "confirmed, so its seed was not admitted: " + stop.detail)
            raise stop
        expired = None
        if kind is not None:
            seed.failed(kind, detail)
        else:
            try:
                # Sealed first: its identity covers the read-only modes every check sees.
                seeds.seal(seed.path, self.seed_bounds, self.sampling_budget)
                seed.ready(*seeds.tree(seed.path, self.seed_bounds, self.sampling_budget))
            except budget.Expired as error:
                seed.failed("driver", "the run's deadline ended the seed's admission: %s"
                            % error)
                expired = error
            except (ContractError, OSError) as error:
                seed.failed("fixture_failed", "%s is not usable: %s" % (what, error))
        self._cleanup_seed_namespace(namespace, seed)
        if expired is not None:
            raise self.deadline_stop(expired)
        if halt:
            raise _halted(supervised, detail)
        if stop is not None:
            raise stop
        return seed

    def _cleanup_seed_namespace(self, namespace, seed):
        if namespace is None:
            return
        problems = namespace.remove()
        if problems:
            note = "the seed writer's private scratch %s was not removed: %s" % (
                namespace.path, "; ".join(problems))
            seed.failed("lifecycle_violation", note)
            raise Stop("cleanup_uncertain", note)

    def check_seed(self, seed, when, record=None):
        """Stop the shard unless the sealed seed is still exactly what was written."""
        try:
            digest = seeds.tree(seed.path, self.seed_bounds, self.sampling_budget)[0]
        except budget.Expired as expired:
            raise self.deadline_stop(budget.Expired("verifying the %s side's seed %s %s: %s"
                                                    % (seed.side, seed.seed_id, when, expired)))
        except (ContractError, OSError) as error:
            digest = "unreadable (%s)" % error
        if digest == seed.digest:
            return
        seed.record["unchanged"] = False
        note = "the %s side's seed %s changed %s: %s, not %s" % (
            seed.side, seed.seed_id, when, digest, seed.digest)
        if record is not None and record["status"] == "ok":
            record["status"] = "failed"
            record["failure"] = {"kind": "lifecycle_violation",
                                 "detail": oracle.bounded("the sample passed; " + note, 4096)}
        raise Stop("seed_changed", note)

    def remove_seeds(self):
        """Remove every seed of the shard and its store; problems are kept for the record."""
        for key in sorted(self.seeds):
            seed = self.seeds[key]
            if seed.path is not None:
                self.seed_cleanup.extend(seeds.remove(seed.path))
        if self.seed_namespace is not None:
            self.seed_cleanup.extend(self.seed_namespace.remove())
        elif self.seed_store is not None:
            self.seed_cleanup.extend(seeds.remove(self.seed_store))


class Seed(object):
    """One side's seed in a shard: its directory, digest and the evidence `shard.json` keeps.
    Its writing cost is recorded here, once; each sample's copy is that sample's setup."""

    def __init__(self, side, seed_id, kind, writer, af_sha, probe_sha):
        self.side = side
        self.seed_id = seed_id
        self.path = None
        self.digest = None
        self.record = {"side": side, "seed": seed_id, "kind": kind, "writer": writer,
                       "status": "failed", "af_sha256": af_sha, "probe_sha256": probe_sha,
                       "digest": None, "files": None, "bytes": None, "writer_wall_ns": None,
                       "host_wall_ns": None, "termination": None, "lifecycle": None,
                       "samples": 0, "unchanged": True, "failure": None}

    def observe(self, result, host_wall_ns, writer_wall_ns):
        self.record.update(
            writer_wall_ns=writer_wall_ns, host_wall_ns=str(host_wall_ns),
            termination=result["termination"]["kind"], lifecycle=result["lifecycle"]["status"])

    def ready(self, digest, files, size):
        self.digest = digest
        self.record.update(status="ready", digest=digest, files=files, bytes=size,
                           failure=None)

    def failed(self, kind, detail):
        self.digest = None
        self.record.update(status="failed", digest=None, files=None, bytes=None,
                           failure={"kind": kind, "detail": oracle.bounded(detail, 4096)})


def _halted(supervised, detail, retain=False):
    """The stop a supervised outcome demands: a forwarded termination signal interrupts the
    shard; an exit not observed unreaped, an unknown status or an uncertain cleanup fails it.
    `retain` keeps what the sample could reach (ADR-0171)."""
    if supervised.result["termination"]["kind"] == "cancelled":
        return Stop("cancelled", detail, "interrupted", retain=retain)
    return Stop("cleanup_uncertain", detail or "the sample's lifecycle stopped the shard",
                retain=retain)


def _remove_work(path):
    """Remove the shard's private work directory: only authority fixture copies live there."""
    for current, dirs, _ in os.walk(path):
        os.chmod(current, 0o700)
        for name in dirs:
            os.chmod(os.path.join(current, name), 0o700)
    shutil.rmtree(path, ignore_errors=True)


def machine_lock(directory=None, register=None, lease=None):
    """The machine's performance coordination lock (ADR-0169), or Unavailable."""
    try:
        return hostlock.acquire(directory, register=register, lease=lease)
    except hostlock.Refused as refused:
        raise Unavailable(refused.reason, str(refused))


def run_shard(options, machine_lease=None):
    """Execute one shard; its terminal `af.perf-shard/4` document.

    The machine lock is taken before anything shared is touched (the run directory, the
    scratch filesystem's enforcement fill, a container) and held until the terminal evidence
    is written, whatever happens."""
    lock = machine_lock(options.lock_directory, lease=machine_lease)
    try:
        if machine_lease is None:
            lock.guard()
        shard = Shard(options, lock.path)
        doc = shard.run()
        if machine_lease is not None and (shard.retained or shard.seed_cleanup):
            lock.mark_uncertain()
        if machine_lease is None and not shard.retained and not shard.seed_cleanup:
            lock.confirm_clean()
        return doc
    except Unavailable as error:
        # Constructor refusal precedes all candidate launches. Its preflight subprocesses
        # have settled through the existing bounded APIs.
        if machine_lease is None and 'shard' not in locals() and error.cleanup_confirmed:
            lock.confirm_clean()
        elif machine_lease is not None and not error.cleanup_confirmed:
            lock.mark_uncertain()
        raise
    finally:
        lock.release()
