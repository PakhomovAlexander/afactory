"""Separate bounded diagnostic child runs of a comparison (package p5, ADR-0174 to ADR-0176).

`cli.py profile --plan RUN_JSON` freezes a child plan (scope `diagnostic`) linked to its parent,
selects cases, builds or verifies the diagnostic recipe's binaries, and captures within the
diagnostic budgets, writing everything to one new child-run directory:

- **Selection.** Explicit `--case` selections are exactly what is asked for, at most eight.
  Automatic selection reads the parent's verdict: regressed cases, then inconclusive ones, each
  class by stable instance ID, three by default and eight at most; the rest are listed as not
  selected.
- **One machine, one supervisor.** The machine's coordination lock (`hostlock`), the one timed
  shards take, is held for the whole child run, and every tool and candidate runs as a request
  to the authority supervisor of the verified build (`toolrun`), in the explicitly selected
  mode, recorded with the containment the plan's environment declares and whether it matches.
  Trusted local mode runs on this host under process-group containment. The contained Linux
  mode takes the shard's own preflight (`runner.contained_host`: the pinned image, a Linux
  runtime with cgroup v2, the scratch filesystem's bound verified by an enforcement fill) and
  runs every request in its own sample container (`toolrun.Contained`): network none, read-only
  root, no capabilities, no new privileges, the environment's PID, memory, swap and CPU set,
  with only the run's own directory of that filesystem writable, so the child's private state,
  seeds and staged outputs are bounded by it. Neither mode replaces the other (ADR-0176).
- **Budgets.** One absolute wall deadline covers every step, from the build to the last
  admission and clean-up; the evidence byte cap is enforced while outputs are produced: each
  file a tool writes is bounded by the room left (`RLIMIT_FSIZE`, perf's `--max-size`), all of
  its files together by a watch that cancels it through its supervisor once they outgrow the
  room, and a step that left the evidence over its room is refused, everything it wrote kept
  and counted (`evidence_over_room`). Every file the driver writes is charged before it is
  written, and the usage recorded is what the evidence holds, never clamped. A declared
  administrative reserve inside the cap holds `profiles.json`, the page and the manifest. The
  case cap and build cap are checked before every step; an exhausted budget stops further work
  and records each unstarted capture as an omission. Only directories this run created are
  removed.
- **Admission.** Every file a tool or candidate produced is admitted as a confined, bounded
  regular file (`admission`) before it is read, hashed, copied or rendered, and a seed tree is
  admitted whole, sealed and identified before its confined copy into each repetition; a
  passing message never admits a file. A supervised run whose stdout was truncated, whose
  lifecycle was refused or whose cleanup is unconfirmed fails before any oracle reads it.
- **Never the verdict.** Nothing here reads or writes the parent's evidence beyond its plan,
  verdict and build manifest, and `profiles.json` records `verdict_effect: none`.
- **Honest states.** A missing or unconfigured tool, an unsupported event, an unobserved region
  or a refused SVG is an omission with its reason and an exact reproduction command, never a
  substitute measurement.

Native library cases are captured on the probe's acknowledged region. A CLI case runs the side's
own AF binary for its command, each execution a fresh process in a private HOME, XDG and TMPDIR
after its declared setup, its seed written once by that side's own writers, and its whole output
observed by the supervisor and judged by the command's oracle; a CLI CPU profile records each
process whole, and its trace's region is the startup or the dispatched command. A case that
spawns processes runs only in its contained environment, and is otherwise an omission naming that
capability.
"""
import os
import socket
import stat
import time
from fractions import Fraction

from . import (admission, builds, bundle, clicases, contracts, counters, diagcontracts,
               flamegraph, hostlock, oracle, perfcapture, phasetrace, render, reproduce, seeds,
               stats, supervise, telemetrydiag, toolrun, tomlfile)
from . import budget as deadlines
from . import limits
from .values import (ContractError, canonical_bytes, format_decimal, format_rational,
                     require, sha256_bytes)

REGISTRY_PATH = "fixtures/performance/diagnostics.toml"
PLAN_FILE = "run.json"
PROFILES_FILE = "profiles.json"
PAGE_FILE = "diagnostic.html"
MANIFEST_FILE = "manifest.json"
WORK_DIR = ".work"
SCRIPT_BYTES_MAX = 64 * 1024 * 1024
# A seed writer's bound, the same as a shard's (ADR-0170), within the diagnostic deadline.
SEED_WRITER_SECONDS = limits.SAMPLE_TIMEOUT_SECONDS_MAX
OMISSION_KIND = {"cpu_profile": "cpu_profile", "trace": "trace", "counters": "counter",
                 "telemetry": "telemetry"}
# The environment variable that selects a telemetry capture's level (docs/telemetry.md).
TELEMETRY_LEVEL = "AF_TELEMETRY__LEVEL"
TRACE_BYTES_MAX = 16 * 1024 * 1024
# The part of the byte cap kept for the administrative record: profiles.json, the page and the
# bundle manifest, written after the last evidence (ADR-0175).
ADMINISTRATIVE_RESERVE_BYTES = 8 * 1024 * 1024
# Admission bounds of one directory of evidence a tool wrote.
EVIDENCE_ENTRIES_MAX = 4096
EVIDENCE_DEPTH_MAX = 8
BUILD_ID_LIST_BYTES = 256 * 1024
# A step whose outputs, all its files together, went over the evidence room it was given:
# refused, everything it wrote kept and counted (ADR-0176).
OVER_ROOM = "evidence_over_room"
TRUSTED_LOCAL = "trusted_local"
LINUX_CONTAINER = "linux_container"
MODES = (TRUSTED_LOCAL, LINUX_CONTAINER)
# A CLI command's environment, as a shard's (`runner.CLI_ENVIRONMENT`), with its private
# HOME, XDG directories and TMPDIR and the case's declared AF settings added.
CLI_ENVIRONMENT = clicases.ENVIRONMENT
# A CLI fixture's untimed setup or seed writer: the bound a shard gives it (ADR-0169).
CLI_SETUP_SECONDS = limits.SAMPLE_TIMEOUT_SECONDS_MAX
CLI_SEED_LOG_BYTES = 64 * 1024
TRACE_OUTPUT = "AF_PERF_TRACE_OUTPUT"
# Where an execution's raw trace is written while it runs: below the evidence (its stage when
# contained), so the evidence counter and the watch count its bytes as they are written; never
# the execution's private scratch, which is not evidence.
TRACE_STAGE = ".trace-stage"


class CliAuthority(object):
    """What a CLI case's diagnostics read from the plan's authority: its CLI registry
    (`fixtures/performance/cli.toml`), the materialized fixture directory with `setup.sh`, each
    side's own package version from its Cargo.lock and the adapter chosen for it (ADR-0168)."""

    def __init__(self, doc, fixtures, versions, adapters):
        self.doc = doc
        self.fixtures = fixtures
        self.versions = versions
        self.adapters = adapters


def cli_region_phase(case):
    """The phase of a CLI process's traced region: its startup for a discovery command, which
    prints and exits before any command is dispatched, otherwise its dispatched command."""
    return "cli_startup" if case["case_id"] == "cli_discovery" else "cli_command"


def trace_coverage(doc, phase):
    """(spans of `phase`, coverage) of a validated trace: `region_missing` when none was kept
    (a recorder its setup filled), `partial` when spans were dropped or left open, `complete`
    otherwise (ADR-0176)."""
    spans = sum(1 for span in doc["spans"] if span["phase"] == phase)
    if not spans:
        return 0, "region_missing"
    if doc["dropped"] or doc["open"]:
        return spans, "partial"
    return spans, "complete"


def load_bytes(data, where=REGISTRY_PATH, suite_doc=None):
    return diagcontracts.validate_diagnostics(tomlfile.loads(data, where), where, suite_doc)


def load(path, suite_doc=None):
    with open(path, "rb") as stream:
        return load_bytes(stream.read(limits.MAX_JSON_BYTES + 1), path, suite_doc)


# Selection ---------------------------------------------------------------------------------

def select(plan, verdict=None, explicit=(), limit=None, verdict_sha256=None):
    """The child run's selection record and the selected plan cases, in order."""
    by_id = dict((case["instance_id"], case) for case in plan["cases"])
    if explicit:
        require(verdict is None, "--case and --verdict: an explicit selection reads no verdict")
        names = list(explicit)
        require(len(names) == len(set(names)), "--case: each case once")
        require(len(names) <= limits.DIAGNOSTIC_CASES_MAX,
                "--case: at most %d diagnostic cases" % limits.DIAGNOSTIC_CASES_MAX)
        for name in names:
            require(name in by_id, "--case %s: not an instance of the parent run" % name)
        record = {"mode": "explicit", "limit": limit or limits.DIAGNOSTIC_CASES_MAX,
                  "parent_verdict_sha256": None,
                  "cases": [{"instance_id": name, "case_id": by_id[name]["case_id"],
                             "reason": "explicit"} for name in names],
                  "not_selected": []}
        require(len(names) <= record["limit"], "--max-cases: fewer than the named cases")
        return record, [by_id[name] for name in names]
    require(verdict is not None and verdict_sha256 is not None,
            "automatic selection reads the parent's verdict (--verdict VERDICT_JSON)")
    require(verdict["run_id"] == plan["run_id"] and verdict["plan_digest"] == plan["plan_digest"],
            "--verdict: of another run or plan than --plan")
    limit = limits.DIAGNOSTIC_CASES_DEFAULT if limit is None else limit
    require(1 <= limit <= limits.DIAGNOSTIC_CASES_MAX,
            "--max-cases: between 1 and %d" % limits.DIAGNOSTIC_CASES_MAX)
    ranked = []
    for entry in verdict["cases"]:
        classification = entry["statistical_classification"]
        if classification == stats.REGRESSION:
            ranked.append((0, entry["instance_id"], "regressed"))
        elif classification == stats.INCONCLUSIVE:
            ranked.append((1, entry["instance_id"], "inconclusive"))
    ranked.sort()
    chosen, rest = ranked[:limit], ranked[limit:]
    record = {"mode": "automatic", "limit": limit, "parent_verdict_sha256": verdict_sha256,
              "cases": [{"instance_id": name, "case_id": by_id[name]["case_id"],
                         "reason": reason} for _, name, reason in chosen],
              "not_selected": [{"instance_id": name, "reason": "over_case_limit"}
                               for _, name, _ in rest]}
    return record, [by_id[name] for _, name, _ in chosen]


# Budgets -----------------------------------------------------------------------------------

class Exhausted(Exception):
    def __init__(self, reason, detail):
        Exception.__init__(self, detail)
        self.reason = reason
        self.detail = detail


def explicit(value, name, lowest, highest):
    """An explicitly given bound, distinguished from an absent one (None): zero, a negative
    value or one beyond its hard maximum is refused before anything is created or launched."""
    if value is None:
        return None
    require(isinstance(value, int) and not isinstance(value, bool) and lowest <= value
            <= highest, "%s: %r is not between %d and %d" % (name, value, lowest, highest))
    return value


class Budget(object):
    """The child run's wall deadline, evidence byte cap and build cap, enforced while it runs."""

    def __init__(self, registry_limits, root, clock=time.monotonic, wall=time.time,
                 total_minutes=None, profile_bytes=None):
        self.clock = clock
        self.wall = wall
        minutes = explicit(total_minutes, "--total-minutes", 1,
                           limits.DIAGNOSTIC_RUN_MINUTES_MAX)
        minutes = registry_limits["total_minutes"] if minutes is None else minutes
        self.bytes_limit = explicit(profile_bytes, "--profile-bytes", 1,
                                    limits.DIAGNOSTIC_BYTES_MAX)
        if self.bytes_limit is None:
            self.bytes_limit = registry_limits["profile_bytes"]
        self.reserve = min(ADMINISTRATIVE_RESERVE_BYTES, self.bytes_limit // 2)
        self.evidence_limit = self.bytes_limit - self.reserve
        self.builds_limit = registry_limits["builds"]
        self.tool_seconds = registry_limits["tool_seconds"]
        self.tool_output_bytes = registry_limits["tool_output_bytes"]
        self.started = clock()
        self.started_unix_ms = int(wall() * 1000)
        self.wall_limit_ms = minutes * 60000
        self.deadline = self.started + minutes * 60
        self.deadline_unix_ms = self.started_unix_ms + self.wall_limit_ms
        self.root = root
        # A contained run's outputs staged on the verified scratch filesystem before they are
        # admitted into the evidence: counted with it, never apart (ADR-0176).
        self.stage = None
        self.builds_used = 0

    def remaining(self):
        return self.deadline - self.clock()

    def used_bytes(self):
        """The evidence bytes the child run holds now, every file's logical size counted and
        nothing clamped, with any outputs staged for it; its own `.work` scratch is not evidence.
        Anything in the evidence that is not a directory or a regular file refuses."""
        total = 0
        bounds = admission.Bounds(EVIDENCE_ENTRIES_MAX * 16, EVIDENCE_DEPTH_MAX,
                                  limits.DIAGNOSTIC_BYTES_MAX * 4,
                                  limits.DIAGNOSTIC_BYTES_MAX * 4)
        if self.stage is not None and os.path.isdir(self.stage):
            total += sum(entry.size for entry in admission.scan(self.stage, bounds,
                                                                label=self.stage))
        for name in sorted(os.listdir(self.root)):
            if name == WORK_DIR:
                continue
            path = os.path.join(self.root, name)
            info = os.lstat(path)
            if stat.S_ISDIR(info.st_mode):
                total += sum(entry.size for entry in admission.scan(path, bounds, label=path))
            else:
                require(stat.S_ISREG(info.st_mode), "the child run's evidence holds %s, which "
                        "is not a regular file" % path)
                total += info.st_size
        return total

    def room(self):
        """Evidence bytes still free before the administrative reserve."""
        return self.evidence_limit - self.used_bytes()

    def check(self, what):
        """Refuse to start `what` once the deadline or the evidence room is spent."""
        if self.remaining() <= 0:
            raise Exhausted("diagnostic_deadline", "the %d-minute diagnostic deadline passed "
                            "before %s" % (self.wall_limit_ms // 60000, what))
        used = self.used_bytes()
        if used >= self.evidence_limit:
            raise Exhausted("profile_bytes_exhausted", "the child run's evidence holds %d "
                            "bytes, its %d-byte room within the %d-byte cap, before %s"
                            % (used, self.evidence_limit, self.bytes_limit, what))

    def charge(self, size, what):
        """Refuse writing `size` more bytes of evidence beyond the room left."""
        room = self.room()
        if size > room:
            raise Exhausted("profile_bytes_exhausted", "%s needs %d bytes, and %d are left of "
                            "the evidence room" % (what, size, max(room, 0)))

    def tool_deadline(self):
        return min(self.deadline, self.clock() + self.tool_seconds)

    def build(self, sides):
        if self.builds_used + sides > self.builds_limit:
            raise Exhausted("build_cap", "a build of %d sides exceeds the %d-build cap"
                            % (sides, self.builds_limit))
        self.builds_used += sides

    def record(self, cases):
        used = self.used_bytes()
        return {"wall_ms": {"limit": self.wall_limit_ms,
                            "used": int((self.clock() - self.started) * 1000)},
                "bytes": {"limit": self.bytes_limit, "administrative_reserve": self.reserve,
                          "evidence_used": used,
                          "evidence_overage": max(0, used - self.evidence_limit)},
                "cases": {"limit": limits.DIAGNOSTIC_CASES_MAX, "used": cases},
                "builds": {"limit": self.builds_limit, "used": self.builds_used}}


# Tools -------------------------------------------------------------------------------------

TOOL_ENV = {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "TZ": "UTC"}


def probe_tools(registry, configured, host_os, names, supervisor=None, deadline=None,
                seconds=30, cwd="/", binds=None):
    """{name: (record, path or None)} for the tools in `names`: each configured executable's
    SHA-256 and the version it reports, or why it is unavailable with its provisioning. A
    version is asked under `supervisor`, never by a process this driver starts itself;
    `binds(path)`, when given, are the binds of a contained request running that executable."""
    found = {}
    for name in names:
        tool = registry["tools"][name]
        record = {"name": name, "state": "unavailable", "executable_sha256": None,
                  "package": tool["package"], "declared_version": tool.get("version"),
                  "reported_version": None,
                  "version_provenance": "declared_pin_only" if "version" in tool else "unknown",
                  "detail": "", "provisioning": tool["provisioning"]}
        path = configured.get(name)
        if host_os not in tool["os"]:
            record["detail"] = "%s does not run on %s" % (tool["executable"], host_os)
        elif path is None:
            record["detail"] = "not configured on this executor"
        elif not (os.path.isfile(path) and os.access(path, os.X_OK)):
            record["detail"] = "%s is not an executable file" % path
        elif perfcapture.executable_identity(path) is None:
            record["detail"] = "%s is larger than any configured tool may be" % path
        else:
            record["state"] = "available"
            record["executable_sha256"] = perfcapture.executable_identity(path)
            if "version_argv" in tool and supervisor is None:
                record["detail"] = "no supervisor was available to ask its version"
            elif "version_argv" in tool:
                result = toolrun.run(supervisor, [path] + tool["version_argv"], TOOL_ENV,
                                     seconds, 4096, cwd=cwd, deadline=deadline,
                                     binds=binds(path) if binds is not None else ())
                text = result.stdout.decode("utf-8", "replace").strip().splitlines()
                if result.failure() is None and text:
                    record["reported_version"] = text[0][:256]
                    record["version_provenance"] = "tool_reported"
                else:
                    record["detail"] = "it did not report a version: %s" % (
                        result.failure() or "empty output")
            else:
                record["detail"] = ("it reports no version; the declared pin and its "
                                    "SHA-256 identify it")
        found[name] = (record, path if record["state"] == "available" else None)
    return found


def configured_tools(perf=None, inferno_bin=None, samply=None, valgrind_bin=None):
    """Executables by registry name from explicit configuration only; nothing is searched for
    on PATH."""
    out = {}
    if perf:
        out["perf"] = os.path.abspath(perf)
    if inferno_bin:
        for name, executable in (("inferno_collapse_perf", "inferno-collapse-perf"),
                                 ("inferno_diff_folded", "inferno-diff-folded"),
                                 ("inferno_flamegraph", "inferno-flamegraph")):
            out[name] = os.path.join(os.path.abspath(inferno_bin), executable)
    if samply:
        out["samply"] = os.path.abspath(samply)
    if valgrind_bin:
        out["valgrind"] = os.path.join(os.path.abspath(valgrind_bin), "valgrind")
        out["callgrind_control"] = os.path.join(os.path.abspath(valgrind_bin),
                                                "callgrind_control")
    return out


# The child run -----------------------------------------------------------------------------

def _ms(seconds):
    """Exact milliseconds of a monotonic interval, to the microsecond."""
    return format_decimal(Fraction(int(round(max(seconds, 0) * 1e6)), 1000))


# A run being launched: until its Result says otherwise, anything it started may be live.
LAUNCHING = object()




def _uncertain(result):
    """Whether a namespace a run used may still be reached by a process it started: nothing
    was launched (None), a launch did not return, or its supervisor did not confirm the end."""
    return result is LAUNCHING or (result is not None and result.uncertain)


STOPPED = {"capped": "recording_cap", "single": "single_repetition",
           "window": "supervised_window"}


class Profile(object):
    """One diagnostic child run. `probes[side]` and `parent_probes[side]` are (path, sha256) of
    the diagnostic and the verdict build's probe; `authority` holds the registry doc and golden
    entries the probe is judged by; `supervisor` is the verified authority supervisor every
    execution runs under (`toolrun.Supervisor`)."""

    def __init__(self, output, parent_plan, plan, registry, registry_sha256, recipe_id,
                 selection, cases, authority_doc, golden, probes, parent_probes, tools,
                 budget, reproduction, builds_sha256=None, host_os=None, omissions=(),
                 clock=time.monotonic, supervisor=None, execution=None, cli=None, afs=None,
                 parent_afs=None, work=None, telemetry=None):
        self.output = output
        self.parent_plan = parent_plan
        self.plan = plan
        self.registry = registry
        self.registry_sha256 = registry_sha256
        self.recipe_id = recipe_id
        self.recipe = registry["recipes"][recipe_id]
        # The kind its evidence is classified by is the one its plan froze (ADR-0179).
        require(plan["profile"]["recipe_id"] == recipe_id and plan["diagnostic"]["recipe"]
                == diagcontracts.frozen_identity(registry, recipe_id),
                "the child plan does not freeze diagnostic recipe %s as the registry declares "
                "it" % recipe_id)
        self.selection = selection
        self.cases = cases
        self.doc = authority_doc
        self.golden = golden
        self.probes = probes
        self.parent_probes = parent_probes
        self.tools = tools
        self.budget = budget
        self.reproduction = reproduction
        self.builds_sha256 = builds_sha256
        self.host_os = host_os
        self.clock = clock
        self.supervisor = supervisor
        self.execution = dict({"mode": TRUSTED_LOCAL, "containment": "process_group",
                               "declared_containment": "trusted_local_process_group",
                               "matches_environment": True, "supervisor_sha256": None,
                               "machine_lock": None,
                               "scratch_bound": "not_enforced_in_trusted_local_mode",
                               "container": None}, **(execution or {}))
        self.captures, self.renders, self.traces, self.counters = [], [], [], []
        # The explicitly selected telemetry level and each telemetry capture (ADR-0214).
        self.telemetry_level = telemetry
        self.telemetry = []
        # Each trace entry's validated document, for the page's timeline; None when omitted.
        self.trace_docs = []
        # Scratch whose removal was not confirmed: kept, and so is the work directory.
        self.retained = []
        self.omissions = list(omissions)
        self.seeds = {}
        self.seed_failures = {}
        # The run's own work: below its output in trusted local mode, and its own directory of
        # the verified scratch filesystem when contained, where every output a tool writes is
        # staged (`produced`) and admitted into the evidence afterwards (`collect`).
        self.work = work or os.path.join(output, WORK_DIR)
        self.stage = None
        if self.contained():
            self.stage = os.path.join(self.work, "out")
            budget.stage = self.stage
        # The CLI authority (`CliAuthority`), and {side: (path, sha256)} of each side's
        # diagnostic and verdict AF binary, for process cases.
        self.cli = cli
        self.afs = afs or {}
        self.parent_afs = parent_afs or {}
        capture = registry["capture"]
        diagnostic = plan["diagnostic"]
        self.frequency = diagnostic["frequency_hz"]
        self.duration_ms = diagnostic["duration_ms"]
        self.event = capture["event"]

    # Bookkeeping -------------------------------------------------------------------------

    def omit(self, kind, case, side, reason, detail, reproduce=True):
        self.omissions.append({
            "kind": kind, "instance_id": case["instance_id"] if case else None, "side": side,
            "reason": reason, "detail": detail[:4096],
            "reproduction": (self.reproduction(case["instance_id"] if case else None)
                             if reproduce else None)})

    def tool(self, name):
        entry = self.tools.get(name)
        return entry[1] if entry is not None else None

    def directory(self, *parts):
        path = os.path.join(self.output, *parts)
        os.makedirs(path, exist_ok=True)
        return path

    def contained(self):
        return self.execution["mode"] == LINUX_CONTAINER

    def produced(self, relative):
        """Where a tool writes the new evidence file `relative`: the evidence itself in trusted
        local mode, the run's stage on the verified scratch filesystem when contained."""
        root = self.stage if self.contained() else self.output
        path = os.path.join(root, relative)
        os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
        return path

    def collect(self, relative):
        """Move what a contained step staged for the evidence path `relative`, a file or a
        directory, into the evidence: enumerated as confined, bounded directories and regular
        files and read as enumerated (`admission.copy_into`), then removed from the stage. Its
        bytes were already counted while staged. Nothing to do in trusted local mode."""
        self.halted("collecting " + relative)
        if not self.contained():
            return
        source = os.path.join(self.stage, relative)
        if not os.path.lexists(source):
            return
        bounds = admission.Bounds(EVIDENCE_ENTRIES_MAX, EVIDENCE_DEPTH_MAX,
                                  self.budget.bytes_limit, self.budget.bytes_limit)
        target = os.path.join(self.output, relative)
        if os.path.isdir(source) and not os.path.islink(source):
            os.makedirs(target, exist_ok=True)
            admission.copy_into(source, target, bounds, self.deadlines(), label=relative)
        else:
            os.makedirs(os.path.dirname(target), exist_ok=True)
            fd, info = admission.open_confined(self.stage, relative, relative)
            try:
                require(info.st_size <= self.budget.bytes_limit, "%s holds %d bytes, over the "
                        "%d-byte cap" % (relative, info.st_size, self.budget.bytes_limit))
                admission.copy_fd(fd, info.st_size, target, self.deadlines(), relative)
            finally:
                os.close(fd)
        problems = admission.remove_tree(source)
        if problems:
            raise perfcapture.CaptureError("cleanup_uncertain", "; ".join(problems)[:4096])

    def binds(self, reads=(), writes=()):
        """The (host path, writable) pairs a contained request binds at their own paths: the
        executables and inputs it reads, read-only, and the directories of the run's own work it
        writes; nothing in trusted local mode."""
        if not self.contained():
            return ()
        return tuple((os.path.abspath(path), False) for path in reads if path) + tuple(
            (os.path.abspath(path), True) for path in writes if path)

    # Evidence: admitted, bounded and charged ---------------------------------------------

    def write(self, relative, data):
        """Write new evidence the driver produced, charged before it is written."""
        self.budget.charge(len(data), relative)
        with open(os.path.join(self.output, relative), "xb") as stream:
            stream.write(data)
        return {"bytes": len(data), "sha256": sha256_bytes(data)}

    def admitted(self, root, relative, limit, what):
        """The bytes of the regular file a tool or candidate wrote at `relative` below the
        trusted directory `root`: reached without following a symlink, refused when it is
        anything but a regular file or its logical size is over `limit`, and read within its
        size and the diagnostic deadline (ADR-0175)."""
        self.halted("admitting " + what)
        fd, info = admission.open_confined(root, relative, what)
        try:
            if info.st_size > limit:
                raise Exhausted("profile_bytes_exhausted", "%s holds %d bytes, over its %d-byte "
                                "bound" % (what, info.st_size, limit))
            chunks = []
            admission.read_bounded(fd, info.st_size, self.deadlines(), what, "admitting " + what,
                                   sink=chunks.append)
            return b"".join(chunks)
        finally:
            os.close(fd)

    def deadlines(self):
        return deadlines.Budget(self.budget.deadline, self.clock, 0)

    def artifacts(self, relative):
        """The admitted files of one evidence directory, each hashed within its size."""
        if self.retained:
            return []
        directory = os.path.join(self.output, relative)
        bounds = admission.Bounds(EVIDENCE_ENTRIES_MAX, EVIDENCE_DEPTH_MAX,
                                  self.budget.bytes_limit, self.budget.bytes_limit)
        entries = admission.scan(directory, bounds, self.deadlines(), relative)
        digests = admission.digests(directory, entries, self.deadlines(), relative)
        return sorted(({"path": relative + "/" + entry.path, "bytes": entry.size,
                        "sha256": digests[entry.path]}
                       for entry in entries if entry.kind == "file"),
                      key=lambda item: item["path"])

    def tool_run(self, argv, timeout, cap, cwd, env=None, stdin=b"", stdout_path=None,
                 file_bytes=None, reads=(), writes=(), setup=None, patterns=(), selectors=()):
        """One supervised run of `argv`; a contained one binds the executable and `reads`
        read-only and `writes` writable, and nothing else (ADR-0176)."""
        self.halted(os.path.basename(argv[0]))
        if stdout_path is not None:
            writes = tuple(writes) + (os.path.dirname(stdout_path),)
        binds = self.binds(tuple(reads) + (argv[0],), writes)
        # This shared boundary also owns tools without a Namespace (decoders, renderers,
        # build-id readers). Until a result confirms cleanup, every writable path stays.
        result = LAUNCHING
        error = None
        try:
            result = toolrun.run(self.supervisor, argv, env or TOOL_ENV, timeout, cap, stdin=stdin,
                           cwd=cwd, deadline=self.budget.deadline, clock=self.clock,
                           stdout_path=stdout_path, file_bytes=file_bytes,
                           watch=self.over_room,
                           binds=binds, setup=setup,
                           patterns=patterns, selectors=selectors)
        except BaseException as failure:
            error = failure
        if _uncertain(result):
            recovery = (cwd, self.work, self.stage, os.path.join(self.output, WORK_DIR))
            if setup is not None:
                recovery += (setup[2],)
            paths = sorted(set(os.path.abspath(path) for path in recovery + tuple(writes)
                               if path))
            self.unsafe_staging = sorted(set(getattr(self, "unsafe_staging", ())).union(paths))
            retained = os.path.abspath(cwd or self.work)
            if retained not in self.retained:
                self.retained.append(retained)
            detail = "tool cleanup unconfirmed; retained recovery paths: " + ", ".join(paths)
            if error is not None:
                detail += "; %s: %s" % (type(error).__name__, error)
            raise perfcapture.CaptureError("cleanup_uncertain", detail[:4096]) from error
        if error is not None:
            raise error
        return result

    def over_room(self):
        """While a step runs: why the evidence, all its files together, is over its room, or
        None. Each file is bounded by `RLIMIT_FSIZE`; this bounds their sum as they are written
        (ADR-0176). An entry that cannot be measured now refuses when the step settles."""
        try:
            used = self.budget.used_bytes()
        except (ContractError, OSError):
            return None
        if used <= self.budget.evidence_limit:
            return None
        return ("the evidence held %d bytes while it was written, over its %d-byte room "
                "within the %d-byte cap, so it was cancelled" % (
                    used, self.budget.evidence_limit, self.budget.bytes_limit))

    def settle(self, what):
        """Refuse a finished step whose outputs, all its files together, left the evidence over
        its room: everything it wrote is kept and counted as it is, never deleted or clamped to
        fit, and none of it is admitted as the step's success (ADR-0176)."""
        used = self.budget.used_bytes()
        if used > self.budget.evidence_limit:
            raise Exhausted(OVER_ROOM, "%s left the evidence holding %d bytes, "
                            "over its %d-byte room within the %d-byte cap; what it wrote is "
                            "kept and counted, and none of it is used" % (
                                what, used, self.budget.evidence_limit,
                                self.budget.bytes_limit))

    def tool_output(self, relative, argv, what, limit=SCRIPT_BYTES_MAX):
        """Run a tool whose stdout becomes the new evidence file `relative`, bounded by the
        evidence room while it is written, each file by `RLIMIT_FSIZE` and all of them together
        by the watch; then the step is settled and the file admitted; its bytes."""
        room = min(self.budget.room(), limit)
        if room < 1:
            raise Exhausted("profile_bytes_exhausted", "no evidence room is left for " + what)
        path = self.produced(relative)
        result = self.tool_run(argv, self.budget.tool_seconds, 4096, os.path.dirname(path),
                               stdout_path=path, file_bytes=room, reads=(self.output,))
        if result.exceeded is not None:
            raise Exhausted(OVER_ROOM, "%s: %s" % (what, result.exceeded))
        if result.failure() is not None:
            raise perfcapture.CaptureError("tool_failed", "%s: %s" % (what, result.failure()))
        self.settle(what)
        self.collect(relative)
        return self.admitted(self.output, relative, room, what)

    # Probe executions --------------------------------------------------------------------

    def namespace(self):
        self.halted("a new execution")
        os.makedirs(self.work, mode=0o700, exist_ok=True)
        os.chmod(self.work, 0o700)
        return oracle.Namespace(self.work)

    def release(self, namespace, uncertain=False):
        """End one execution's namespace. Its lifecycle is checked before anything is removed:
        when a process the run started may still be live, the namespace and every byte in it
        are kept for the operator, its handles closed and nothing of it removed, and the run
        stops launching (`halted`). Only a confirmed end lets the namespace be removed, and a
        removal that is not confirmed keeps what is left the same way."""
        if uncertain or self.retained or getattr(namespace, "reader_uncertain", False):
            path = namespace.keep()
            if path not in self.retained:
                self.retained.append(path)
            raise perfcapture.CaptureError(
                "cleanup_uncertain", ("a process the run started may still be live, so its "
                                      "scratch %s was kept for recovery and nothing more of "
                                      "this run is launched" % namespace.path)[:4096])
        problems = namespace.remove()
        if problems:
            # Kept for the operator, never removed beneath a live process.
            self.retained.append(namespace.path)
            raise perfcapture.CaptureError("cleanup_uncertain", "; ".join(problems)[:4096])

    def halted(self, what):
        """Refuse to start `what` once scratch was kept for recovery: a process of an earlier
        execution may still be live, so the run uses nothing more of its work."""
        if getattr(self, "retained", ()):
            raise perfcapture.CaptureError(
                "cleanup_uncertain", ("%s was not started: scratch kept for recovery (%s) may "
                                      "still be reached by a live process"
                                      % (what, ", ".join(self.retained)))[:4096])

    def seed(self, side, case, cli_seed=None):
        """The side's native seed, written once by its own probe and judged (ADR-0170), or with
        `cli_seed` that CLI seed of its own writers; (path, digest) of the sealed seed."""
        key = (side, "cli:" + cli_seed) if cli_seed else (side, case["seed"]["seed_id"])
        if key in self.seeds:
            return self.seeds[key]
        if key in self.seed_failures:
            # Written once, never retried: every case reading it fails with the writer.
            raise self.seed_failures[key]
        try:
            if cli_seed:
                self.seeds[key] = self.write_cli_seed(side, cli_seed, case)
            else:
                self.seeds[key] = self.write_seed(side, case["seed"], key)
        except perfcapture.CaptureError as error:
            self.seed_failures[key] = error
            raise
        return self.seeds[key]

    def write_seed(self, side, declared, key):
        store = os.path.join(self.work, "seeds")
        os.makedirs(store, 0o700, exist_ok=True)
        path = os.path.join(store, "%s-%s" % key)
        os.mkdir(path, 0o700)
        request = oracle.seed_request(self.doc, declared["seed_id"], path)
        namespace = self.namespace()
        result = None
        try:
            result = LAUNCHING
            result = self.tool_run([self.probes[side][0]], SEED_WRITER_SECONDS,
                                   limits.RECORD_BYTES_MAX, namespace.path,
                                   env=supervise.probe_environment(namespace.path),
                                   stdin=canonical_bytes(request),
                                   writes=(namespace.path, path))
        finally:
            self.release(namespace, _uncertain(result))
        if result.supervision_failure() is not None:
            raise perfcapture.CaptureError("seed_failed", result.supervision_failure())
        judged = oracle.judge(self.doc, self.golden, request, result.code, result.stdout)
        if judged["status"] != "passed":
            raise perfcapture.CaptureError("seed_failed", "the %s side's seed writer failed: %s"
                                           % (side, judged["failure"]["detail"]))
        # A passing message admits no file: the whole tree the writer left is enumerated as
        # confined, bounded directories and regular files before anything reads it, then
        # sealed and identified, as a shard's seed is (ADR-0172, ADR-0176).
        try:
            seeds.seal(path, self.seed_bounds(), self.deadlines())
            return path, seeds.tree(path, self.seed_bounds(), self.deadlines())[0]
        except deadlines.Expired as expired:
            raise perfcapture.CaptureError("diagnostic_deadline", str(expired))
        except (ContractError, OSError) as error:
            raise perfcapture.CaptureError("seed_unadmitted", "the %s side's written seed is "
                                           "not admitted: %s" % (side, error))

    def seed_bounds(self):
        """A seed's bounds: the declared entries and depth, and no more logical bytes than the
        scratch of the plan's environment (`seeds.bounds`)."""
        return seeds.bounds(self.plan["profile"]["environment"]["scratch_limit_bytes"])

    def copy_seed(self, side, case, target, cli_seed=None):
        """A new private copy of the side's sealed seed at `target`, through the confined,
        bounded copy (`admission.copy_tree`): nothing but its own directories and regular files
        is read, within its bounds and the deadline, and the bytes copied must be the very seed
        that was admitted."""
        seed, digest = self.seed(side, case, cli_seed)
        try:
            entries, hashes = admission.copy_tree(
                seed, target, self.seed_bounds(), self.deadlines(),
                label="the %s side's seed" % side)
        except deadlines.Expired as expired:
            raise perfcapture.CaptureError("diagnostic_deadline", str(expired))
        except (ContractError, OSError) as error:
            raise perfcapture.CaptureError("seed_unadmitted", "the %s side's seed was not "
                                           "copied: %s" % (side, error))
        copied = seeds.identity(entries, hashes)[0]
        if copied != digest:
            raise perfcapture.CaptureError("seed_changed", "the %s side's seed is %s, not the "
                                           "%s that was admitted" % (side, copied, digest))

    def prepared(self, side, case, namespace):
        """The request of one fresh repetition in `namespace`, its seed copied first. The
        probe's own scratch is the new empty `probe/` it requires; the request, the FIFOs and
        the seed copy sit beside it."""
        scratch = os.path.join(namespace.path, "probe")
        os.mkdir(scratch, 0o700)
        seed_copy = None
        if case["seed"] is not None:
            self.copy_seed(side, case, os.path.join(namespace.path, "seed"))
            # The writer wrote below its own work directory, as a shard's copy reads it.
            seed_copy = os.path.join(namespace.path, "seed", oracle.WORK_DIR)
        request = oracle.request_for(self.doc, case["instance_id"], scratch, seed_copy)
        path = os.path.join(namespace.path, "request.json")
        with open(path, "wb") as stream:
            stream.write(canonical_bytes(request))
        return request, path

    def judged(self, request, code, stdout, what):
        judged = oracle.judge(self.doc, self.golden, request, code, stdout)
        if judged["status"] != "passed":
            raise perfcapture.CaptureError("oracle_failed", "%s failed the oracle: %s"
                                           % (what, judged["failure"]["detail"]))
        return judged

    def refresh(self, side, case, namespace):
        """Replace a repetition's scratch and seed copy with fresh ones, between repetitions of
        one recording; the last repetition's process image has ended."""
        scratch = os.path.join(namespace.path, "probe")
        problems = admission.remove_tree(scratch)
        if problems:
            raise perfcapture.CaptureError("cleanup_uncertain", "; ".join(problems)[:4096])
        os.mkdir(scratch, 0o700)
        if case["seed"] is not None:
            seed_copy = os.path.join(namespace.path, "seed")
            problems = admission.remove_tree(seed_copy)
            if problems:
                raise perfcapture.CaptureError("cleanup_uncertain", "; ".join(problems)[:4096])
            self.copy_seed(side, case, seed_copy)

    def bracketed(self, side, case, argv_for, on_begin, on_end, capture_seconds, repeat=True,
                  messages=None, file_bytes=None, reads=(), writes=()):
        """One recording under the region handshake, its repetitions fresh executions on fresh
        state while the capture's duration, the repetition cap, the supervised window and the
        budgets allow; (Bracketed, judged). Each repetition's message is judged as it arrives
        and, with `messages`, appended to that evidence file, charged first."""
        namespace = self.namespace()
        uncertain = False
        stream = None
        try:
            request, request_path = self.prepared(side, case, namespace)
            fifo = os.path.join(namespace.path, "fifo")
            os.mkdir(fifo, 0o700)
            argv = argv_for(fifo)
            stopped, why, judged = [], {}, []
            if messages is not None:
                stream = open(os.path.join(self.output, messages), "xb")

            def on_record(index, message):
                if stream is not None:
                    self.budget.charge(len(message), messages)
                    stream.write(message)
                    stream.flush()
                judged.append(self.judged(request, 0, message, "repetition %d of the "
                                          "profiled probe" % (index + 1)))

            def on_next(run):
                region = sum(Fraction(item["metrics"]["wall_ms"]["value"]) for item in judged)
                if region >= capture_seconds * 1000:
                    why["stop"] = "duration_reached"
                    return False
                if len(judged) >= self.registry["capture"]["max_repetitions"]:
                    why["stop"] = "repetition_cap"
                    return False
                try:
                    self.budget.check("repetition %d of %s %s" % (len(judged) + 1,
                                                                 case["instance_id"], side))
                except Exhausted as error:
                    why["stop"] = "diagnostic_budget"
                    stopped.append(error)
                    return False
                self.refresh(side, case, namespace)
                return True

            try:
                uncertain = True
                run = perfcapture.bracketed_run(
                    self.supervisor, argv, supervise.probe_environment(namespace.path),
                    namespace.path, request_path, fifo, on_begin, on_end, on_record,
                    capture_seconds, self.budget.deadline,
                    self.registry["capture"]["handshake_timeout_seconds"],
                    min(self.budget.tool_seconds, toolrun.TIMEOUT_MS_MAX / 1000.0),
                    on_next=on_next if repeat else None, file_bytes=file_bytes,
                    clock=self.clock, watch=self.over_room,
                    binds=self.binds(tuple(reads) + (argv[0], self.probes[side][0]),
                                     tuple(writes) + (namespace.path,)))
            except perfcapture.CaptureError as error:
                uncertain = error.reason == "cleanup_uncertain"
                raise
            except Exhausted as error:
                # Raised while it ran: the recording was cancelled and its end confirmed.
                uncertain = False
                raise perfcapture.CaptureError(error.reason, error.detail)
            uncertain = False
            run.why = STOPPED.get(run.stopped) or why.get("stop", "single_repetition")
            if stopped:
                self.omit(OMISSION_KIND[self.recipe["kind"]], case, side, stopped[0].reason,
                          "stopped after %d repetitions: %s" % (len(judged),
                                                                stopped[0].detail))
            if len(judged) != len(run.regions):
                raise perfcapture.CaptureError("oracle_failed", "%d messages for %d bracketed "
                                               "repetitions" % (len(judged), len(run.regions)))
            run.region_ms = sum(Fraction(item["metrics"]["wall_ms"]["value"]) for item in judged)
            return run, judged
        finally:
            if stream is not None:
                stream.close()
            self.release(namespace, uncertain)

    # CPU profiles with perf --------------------------------------------------------------

    def perf_capture(self, case, side, attempt, unwind):
        perf = self.tool("perf")
        relative = "captures/%s/%s/%d" % (case["instance_id"], side, attempt)
        directory = self.directory(relative)
        record = {"instance_id": case["instance_id"], "side": side, "attempt": attempt,
                  "tool": "perf", "subject": "probe_region",
                  "binary_sha256": self.probes[side][1], "event": self.event,
                  "frequency_hz": self.frequency, "duration_ms": self.duration_ms,
                  "effective_frequency_hz": None, "unwind": unwind,
                  "dwarf_stack_bytes": (limits.DIAGNOSTIC_DWARF_STACK_BYTES
                                        if unwind == "dwarf" else None),
                  "region": {"mechanism": "acknowledged_handshake", "process": "fresh_exec",
                             "repetitions": 0, "setup_ms": "0", "region_ms": "0",
                             "enabled_ms": "0", "wall_ms": "0", "observed": False,
                             "truncated_at_cap": False, "stopped": "not_started"},
                  "samples": None, "losses": perfcapture.unknown_losses("nothing recorded"),
                  "frames": None, "unknown_frames": None, "unknown_frame_ratio": None,
                  "symbols": {"state": "missing", "build_ids": [],
                              "binary_build_id_present": None},
                  "artifacts": [], "omission": None}
        data = os.path.join(directory, "capture.data")
        # Where perf writes it: the evidence, or the stage when contained (`produced`).
        staged = self.produced(relative + "/capture.data")
        holder = {}
        region = None
        try:
            self.budget.check("the %s capture of %s" % (side, case["instance_id"]))
            room = self.budget.room()

            def argv_for(fifo):
                holder["control"] = perfcapture.PerfControl(fifo)
                return perfcapture.record_argv(perf, self.event, self.frequency, unwind,
                                               staged, holder["control"],
                                               [self.probes[side][0]], max_bytes=room)

            try:
                run, _ = self.bracketed(
                    side, case, argv_for,
                    lambda deadline: holder["control"].send(b"enable\n", deadline),
                    lambda deadline: holder["control"].send(b"disable\n", deadline),
                    self.duration_ms / 1000.0, messages=relative + "/messages.jsonl",
                    file_bytes=room, writes=(os.path.dirname(staged),))
            finally:
                if "control" in holder:
                    holder["control"].close()
            region = run.region_ms
            self.settle("the %s recording of %s" % (side, case["instance_id"]))
            record["region"].update(
                repetitions=len(run.regions), observed=bool(run.regions),
                truncated_at_cap=any(item.truncated for item in run.regions),
                setup_ms=_ms(run.setup()), region_ms=format_decimal(region),
                enabled_ms=_ms(run.recorded()), wall_ms=_ms(run.exited - run.launched),
                stopped=run.why)
        except (perfcapture.CaptureError, Exhausted) as error:
            # perf ends its own session at the byte room ("perf size limit reached").
            reason = ("profile_bytes_exhausted" if "size limit reached" in error.detail
                      and error.reason != OVER_ROOM else error.reason)
            record["omission"] = {"reason": reason, "detail": error.detail[:4096]}
        except ContractError as error:
            record["omission"] = {"reason": "probe_refused", "detail": str(error)[:4096]}
        except OSError as error:
            record["omission"] = {"reason": "driver_error", "detail": str(error)[:4096]}
        try:
            # A partial or refused recording is kept too: admitted into the evidence.
            self.collect(relative + "/capture.data")
        except (perfcapture.CaptureError, ContractError, OSError) as error:
            record["omission"] = record["omission"] or {
                "reason": "capture_unadmitted", "detail": str(error)[:4096]}
        if os.path.lexists(data) and (record["omission"] or {}).get("reason") != OVER_ROOM:
            # A recording over its room is kept and counted, never analysed as a capture.
            try:
                self.analyse(record, directory, relative, region)
            except (perfcapture.CaptureError, flamegraph.RenderError, Exhausted) as error:
                record["omission"] = record["omission"] or {"reason": error.reason,
                                                            "detail": error.detail[:4096]}
            except (ContractError, OSError) as error:
                record["omission"] = record["omission"] or {"reason": "capture_unadmitted",
                                                            "detail": str(error)[:4096]}
        return self.finish_capture(record, relative)

    def finish_capture(self, record, relative):
        """A perf capture's last checks: samples to attribute, its admitted artifacts, and the
        record kept with every reason it is not rendered."""
        if record["omission"] is None and not record["samples"]:
            record["omission"] = {"reason": "region_without_samples",
                                  "detail": "the enabled region recorded no %s sample"
                                  % self.event}
        minimum = self.registry["capture"]["minimum_samples"]
        if record["omission"] is None and record["samples"] < minimum:
            record["omission"] = {"reason": "too_few_samples", "detail": (
                "%d samples over %s ms of region in %d repetitions (stopped: %s); at least %d "
                "are needed to attribute it, so the capture is kept and nothing is rendered "
                "from it" % (record["samples"], record["region"]["region_ms"],
                             record["region"]["repetitions"], record["region"]["stopped"],
                             minimum))}
        try:
            record["artifacts"] = self.artifacts(relative)
        except (ContractError, OSError) as error:
            record["artifacts"] = []
            record["omission"] = {"reason": "capture_unadmitted", "detail": str(error)[:4096]}
        if not record["artifacts"] and record["omission"] is None:
            record["omission"] = {"reason": "no_artifacts", "detail": "nothing was captured"}
        self.captures.append(record)
        return record

    def cli_perf_capture(self, case, side, attempt, unwind):
        """A CLI case's CPU profile: each repetition a fresh AF process on fresh private state,
        recorded whole by its own `perf record` and judged by its command's oracle as it ends,
        until the recorded processes' time reaches the capture's duration, the repetition cap
        or the budget; then one script of every recording (ADR-0176)."""
        perf = self.tool("perf")
        relative = "captures/%s/%s/%d" % (case["instance_id"], side, attempt)
        directory = self.directory(relative)
        record = {"instance_id": case["instance_id"], "side": side, "attempt": attempt,
                  "tool": "perf", "subject": "cli_process", "binary_sha256": self.afs[side][1],
                  "event": self.event, "frequency_hz": self.frequency,
                  "duration_ms": self.duration_ms, "effective_frequency_hz": None,
                  "unwind": unwind,
                  "dwarf_stack_bytes": (limits.DIAGNOSTIC_DWARF_STACK_BYTES
                                        if unwind == "dwarf" else None),
                  "region": {"mechanism": "whole_process", "process": "fresh_process",
                             "repetitions": 0, "setup_ms": "0", "region_ms": "0",
                             "enabled_ms": "0", "wall_ms": "0", "observed": False,
                             "truncated_at_cap": False, "stopped": "not_started"},
                  "samples": None, "losses": perfcapture.unknown_losses("nothing recorded"),
                  "frames": None, "unknown_frames": None, "unknown_frame_ratio": None,
                  "symbols": {"state": "missing", "build_ids": [],
                              "binary_build_id_present": None},
                  "artifacts": [], "omission": None}
        names, region, setup_ns, wall, stop = [], Fraction(0), 0, 0.0, "not_started"
        try:
            while True:
                if region >= self.duration_ms:
                    stop = "duration_reached"
                    break
                if len(names) >= self.registry["capture"]["max_repetitions"]:
                    stop = "repetition_cap"
                    break
                try:
                    self.budget.check("repetition %d of %s %s" % (len(names) + 1,
                                                                 case["instance_id"], side))
                except Exhausted as error:
                    stop = "diagnostic_budget"
                    if names:
                        self.omit("cpu_profile", case, side, error.reason, "stopped after %d "
                                  "repetitions: %s" % (len(names), error.detail))
                        break
                    raise
                name = "rep-%04d.data" % (len(names) + 1)
                staged = self.produced(relative + "/" + name)
                room = self.budget.room()

                def wrap(argv, staged=staged, room=room):
                    return perfcapture.whole_argv(perf, self.event, self.frequency, unwind,
                                                  staged, argv, max_bytes=room)

                started = self.clock()
                try:
                    result, value, _ = self.cli_run(
                        side, case, self.afs[side][0], wrap=wrap, file_bytes=room,
                        writes=(os.path.dirname(staged),),
                        what="repetition %d of the recorded command" % (len(names) + 1))
                finally:
                    wall += self.clock() - started
                    self.collect(relative)
                self.settle("repetition %d of %s %s" % (len(names) + 1, case["instance_id"],
                                                        side))
                names.append(name)
                region += Fraction(value)
                reply = result.supervised.result
                if reply["setup"] is not None and reply["setup"]["wall_ns"] is not None:
                    setup_ns += int(reply["setup"]["wall_ns"])
        except (perfcapture.CaptureError, Exhausted) as error:
            reason = ("profile_bytes_exhausted" if "size limit reached" in error.detail
                      and error.reason != OVER_ROOM else error.reason)
            record["omission"] = {"reason": reason, "detail": error.detail[:4096]}
        except ContractError as error:
            record["omission"] = {"reason": "cli_refused", "detail": str(error)[:4096]}
        except OSError as error:
            record["omission"] = {"reason": "driver_error", "detail": str(error)[:4096]}
        try:
            # A partial or refused recording is kept too: admitted into the evidence.
            self.collect(relative)
        except (perfcapture.CaptureError, ContractError, OSError) as error:
            record["omission"] = record["omission"] or {
                "reason": "capture_unadmitted", "detail": str(error)[:4096]}
        # The recorded processes' own time, perf included: the whole process is the region.
        record["region"].update(
            repetitions=len(names), observed=bool(names), region_ms=format_decimal(region),
            enabled_ms=format_decimal(region), setup_ms=_ms(setup_ns / 1e9), wall_ms=_ms(wall),
            stopped=stop if names else "not_started")
        if names and (record["omission"] or {}).get("reason") != OVER_ROOM:
            try:
                self.analyse(record, directory, relative, region, names=names,
                             binary=self.afs[side])
            except (perfcapture.CaptureError, flamegraph.RenderError, Exhausted) as error:
                record["omission"] = record["omission"] or {"reason": error.reason,
                                                            "detail": error.detail[:4096]}
            except (ContractError, OSError) as error:
                record["omission"] = record["omission"] or {"reason": "capture_unadmitted",
                                                            "detail": str(error)[:4096]}
        return self.finish_capture(record, relative)

    def analyse(self, record, directory, relative, region_ms, names=("capture.data",),
                binary=None):
        """Decode one recording, or the recordings `names` of a CLI capture's fresh processes,
        every tool output admitted before it is read; `binary` is the profiled executable,
        (path, sha256), the side's probe by default."""
        perf = self.tool("perf")
        name = relative + "/" + names[0]
        record["losses"] = self.losses(relative, names)
        self.budget.check("perf script of " + name)
        data = os.path.join(self.output, name)
        if len(names) == 1:
            argv = [perf, "script", "-i", data, "-F", perfcapture.SCRIPT_FIELDS]
        else:
            # One script of every repetition's recording, in order, by one supervised shell.
            argv = [toolrun.SHELL, "-c", 'cd "$1" && for f in rep-*.data; do "$0" script -i '
                    '"$f" -F "$2" || exit 1; done', perf, os.path.join(self.output, relative),
                    perfcapture.SCRIPT_FIELDS]
        script = self.tool_output(relative + "/capture.script", argv,
                                  "perf script of " + relative)
        parsed = perfcapture.parse_script(script.decode("utf-8", "replace"))
        perfcapture.check_event(parsed, self.event)
        listed = self.tool_run([perf, "buildid-list", "--with-hits", "-i", data],
                               self.budget.tool_seconds, BUILD_ID_LIST_BYTES, directory,
                               reads=(self.output,))
        ids = []
        if listed.failure() is None:
            ids = [identity for identity, _ in perfcapture.parse_build_ids(
                listed.stdout.decode("utf-8", "replace")) if identity]
        record.update(samples=parsed.samples, frames=parsed.frames,
                      unknown_frames=parsed.unknown)
        if parsed.frames:
            record["unknown_frame_ratio"] = format_rational(Fraction(parsed.unknown,
                                                                     parsed.frames))
        if parsed.samples and region_ms:
            # Samples per second of the probe-measured region they were taken in.
            record["effective_frequency_hz"] = format_rational(Fraction(parsed.samples * 1000)
                                                               / region_ms)
        side = record["side"]
        profiled = binary or self.probes[side]
        with open(profiled[0], "rb") as stream:
            binary = stream.read(limits.BUILD_ARTIFACT_BYTES_MAX + 1)
        own = perfcapture.elf_build_id(binary)
        unique = sorted(set(ids))[:256]
        symbols = {"state": "referenced", "build_ids": unique,
                   "binary_build_id_present": (own in unique) if own else None}
        target = "symbols/" + profiled[1][7:]
        if len(binary) <= self.budget.room():
            self.directory("symbols")
            if not os.path.lexists(os.path.join(self.output, target)):
                self.write(target, binary)
            symbols["state"] = "retained"
        record["symbols"] = symbols
        if self.inferno_paths() is not None and parsed.samples:
            folded = self.tool_output(relative + "/collapsed.folded",
                                      [self.tool("inferno_collapse_perf"),
                                       os.path.join(self.output, relative, "capture.script")],
                                      "inferno-collapse-perf of " + name)
            try:
                flamegraph.check_folded(folded)
            except flamegraph.RenderError:
                raise

    def losses(self, relative, names):
        """The loss records of every recording `names` below `relative`, decoded from each
        (ADR-0175): the records and payloads of each kind summed over the recordings, never one
        kind into the other, and all unknown when any recording cannot be decoded."""
        totals = dict((key, 0) for key in perfcapture.LOSS_KEYS)
        for name in names:
            path = relative + "/" + name
            fd, info = admission.open_confined(self.output, path, path)
            try:
                decoded = perfcapture.decode_losses(
                    lambda offset, count: os.pread(fd, count, offset), info.st_size)
            finally:
                os.close(fd)
            if decoded["state"] != "decoded":
                return perfcapture.unknown_losses("%s: %s" % (name, decoded["detail"]))
            for key in perfcapture.LOSS_KEYS:
                totals[key] += decoded[key]
        return dict(totals, state="decoded", detail="")

    def inferno_paths(self):
        paths = dict((name, self.tool(name)) for name in flamegraph.TOOLS)
        if any(path is None for path in paths.values()):
            return None
        return paths

    def perf_case(self, case):
        chosen = {}
        threshold = Fraction(self.registry["capture"]["dwarf_when_unknown_ratio_above"])
        capture = self.perf_capture if case["kind"] == "library" else self.cli_perf_capture
        for side in diagcontracts.SIDES:
            first = capture(case, side, 1, "frame_pointer")
            chosen[side] = first
            ratio = (Fraction(first["unknown_frames"], first["frames"])
                     if first["frames"] else None)
            if first["omission"] is None and ratio is not None and ratio > threshold:
                # The one declared alternative; both captures are kept.
                second = capture(case, side, 2, "dwarf")
                if second["omission"] is None:
                    chosen[side] = second
        self.render(case, chosen)

    def render(self, case, chosen):
        paths = self.inferno_paths()
        if paths is None:
            self.omit("flamegraph", case, None, "inferno_unavailable", "the pinned inferno "
                      "tools are not configured on this executor")
            return
        folded = {}
        for side, record in chosen.items():
            relative = "captures/%s/%s/%d/collapsed.folded" % (
                case["instance_id"], side, record["attempt"])
            if record["omission"] is None and os.path.lexists(os.path.join(self.output,
                                                                           relative)):
                folded[side] = relative
            else:
                self.omit("flamegraph", case, side, "no_capture",
                          "the %s side has no usable capture to render" % side)
        if not folded:
            return
        relative = "flamegraphs/" + case["instance_id"]
        try:
            self.budget.check("rendering " + case["instance_id"])
            self.directory(relative)
            self.renders.extend(flamegraph.render_case(
                Renderer(self, paths), relative, case["instance_id"], folded))
        except (flamegraph.RenderError, Exhausted, perfcapture.CaptureError) as error:
            self.omit("flamegraph", case, None, error.reason, error.detail)
        except (ContractError, OSError) as error:
            self.omit("flamegraph", case, None, "render_unadmitted", str(error))

    # samply ------------------------------------------------------------------------------

    def samply_case(self, case):
        """samply's whole-process capture of a probe or, for a CLI case, of the side's AF
        process, each repetition a fresh process judged by its oracle."""
        samply = self.tool("samply")
        cli = case["kind"] != "library"
        subject = "cli_process" if cli else "probe_region"
        binaries = self.afs if cli else self.probes
        for side in diagcontracts.SIDES:
            relative = "captures/%s/%s/1" % (case["instance_id"], side)
            directory = self.directory(relative)
            available, reason, detail, _ = counters.samply_capability(
                self.supervisor, samply, binaries[side][0], TOOL_ENV,
                self.budget.tool_seconds, deadline=self.budget.deadline, cwd=directory)
            if not available:
                self.omit("samply", case, side, reason, detail)
                continue
            record = {"instance_id": case["instance_id"], "side": side, "attempt": 1,
                      "tool": "samply", "subject": subject,
                      "binary_sha256": binaries[side][1], "event": None,
                      "frequency_hz": self.frequency, "duration_ms": self.duration_ms,
                      "effective_frequency_hz": None, "unwind": "frame_pointer",
                      "dwarf_stack_bytes": None,
                      "region": {"mechanism": "whole_process", "process": "fresh_process",
                                 "repetitions": 0, "setup_ms": "0", "region_ms": "0",
                                 "enabled_ms": "0", "wall_ms": "0", "observed": False,
                                 "truncated_at_cap": False, "stopped": "not_started"},
                      "samples": 0,
                      "losses": perfcapture.unknown_losses("samply reports no loss records"),
                      "frames": None, "unknown_frames": None, "unknown_frame_ratio": None,
                      "symbols": {"state": "missing", "build_ids": [],
                                  "binary_build_id_present": None},
                      "artifacts": [], "omission": None}
            wall = 0.0
            stop = "duration_reached"
            try:
                while wall * 1000 < self.duration_ms:
                    if record["region"]["repetitions"] >= \
                            self.registry["capture"]["max_repetitions"]:
                        stop = "repetition_cap"
                        break
                    self.budget.check("samply repetition of " + case["instance_id"])
                    repetition = record["region"]["repetitions"] + 1
                    name = "rep-%04d.json.gz" % repetition
                    output = self.produced(relative + "/" + name)
                    if cli:
                        started = self.clock()
                        try:
                            self.cli_run(side, case, binaries[side][0], wrap=lambda argv: (
                                counters.samply_argv(samply, self.frequency, output, argv)),
                                file_bytes=self.budget.room(),
                                writes=(os.path.dirname(output),),
                                what="samply repetition %d" % repetition)
                        finally:
                            wall += self.clock() - started
                    else:
                        wall += self.samply_probe(side, case, samply, output)
                    self.settle("samply repetition %d of %s" % (repetition,
                                                                 case["instance_id"]))
                    self.collect(relative + "/" + name)
                    counted = counters.samply_samples(self.admitted(
                        directory, name, self.budget.evidence_limit, relative + "/" + name))
                    if counted is None:
                        raise perfcapture.CaptureError("profile_unparsed", "samply's saved "
                                                       "profile is not in a format this reader "
                                                       "knows; it is kept")
                    record["samples"] += counted
                    record["region"].update(repetitions=repetition, observed=True)
            except (perfcapture.CaptureError, Exhausted) as error:
                record["omission"] = {"reason": error.reason, "detail": error.detail[:4096]}
            except (ContractError, OSError) as error:
                record["omission"] = {"reason": "samply_failed", "detail": str(error)[:4096]}
            record["region"].update(region_ms=_ms(wall), enabled_ms=_ms(wall), wall_ms=_ms(wall))
            if record["region"]["repetitions"]:
                record["region"]["stopped"] = stop
            if record["omission"] is None and not record["samples"]:
                record["omission"] = {"reason": "region_without_samples",
                                      "detail": "samply recorded no sample"}
            try:
                record["artifacts"] = self.artifacts(relative)
            except (ContractError, OSError) as error:
                record["artifacts"] = []
                record["omission"] = {"reason": "capture_unadmitted",
                                      "detail": str(error)[:4096]}
            if not record["artifacts"] and record["omission"] is None:
                record["omission"] = {"reason": "no_artifacts", "detail": "nothing was saved"}
            self.captures.append(record)
        self.omit("flamegraph", case, None, "samply_profile_not_collapsed",
                  "samply saves its own profile format, which this laboratory keeps but does "
                  "not collapse into inferno flamegraphs", reproduce=False)

    def samply_probe(self, side, case, samply, output):
        """One samply repetition of the side's probe on fresh state, its message judged; the
        seconds it ran."""
        namespace = self.namespace()
        result = None
        try:
            request, request_path = self.prepared(side, case, namespace)
            started = self.clock()
            result = LAUNCHING
            result = self.tool_run(
                counters.samply_argv(samply, self.frequency, output, [self.probes[side][0]]),
                case["sample_timeout_ms"] / 1000.0, limits.RECORD_BYTES_MAX, namespace.path,
                env=supervise.probe_environment(namespace.path), stdin=canonical_bytes(request),
                file_bytes=self.budget.room(),
                writes=(namespace.path, os.path.dirname(output)))
            elapsed = self.clock() - started
            if result.supervision_failure() is not None:
                raise perfcapture.CaptureError("samply_failed", result.supervision_failure())
            self.judged(request, result.code, result.stdout, "the sampled probe")
            return elapsed
        finally:
            self.release(namespace, _uncertain(result))

    # CLI processes -----------------------------------------------------------------------

    def cli_entry(self, side, case):
        _, adapter = self.cli.adapters[side]
        return clicases.command(adapter, case["case_id"], case["parameters"])

    def cli_environment(self, case, scratch):
        environment = dict(CLI_ENVIRONMENT)
        environment.update(supervise.scratch_environment(scratch))
        environment.update(case.get("af_environment") or {})
        return environment

    def cli_run(self, side, case, binary, wrap=None, writes=(), file_bytes=None, trace=False,
                what="the CLI command", telemetry=None, after=None):
        """One supervised execution of the case's command by the AF `binary` of `side`, in a new
        private namespace that is its HOME, XDG directories and TMPDIR, after the command's
        declared fixture setup; its seed, if it reads one, is that side's own, admitted and
        copied confined first (ADR-0176). `wrap(argv)` puts a profiler around the command. The
        supervisor observes the command's whole stdout, past any retained log, and the command's
        oracle (`clicases.judge`) judges it before anything of the run is used; a lifecycle the
        containment refuses, an unconfirmed cleanup, a timeout or outputs over their room fail
        it first. Returns (Result, the supervised child's wall time in ms as a decimal, the
        admitted trace bytes or None). With `telemetry`, the command records at that level
        (ADR-0214), and `after(namespace, environment)` runs once its oracle passed, in the same
        namespace before it is removed."""
        entry = self.cli_entry(side, case)
        namespace = self.namespace()
        result = staged = room = None
        try:
            scratch = namespace.path
            probe = self.probes[side][0] if side in self.probes else None
            values = {"af": binary, "probe": probe or "", "scratch": scratch,
                      "package_version": self.cli.versions[side]}
            environment = self.cli_environment(case, scratch)
            if telemetry is not None:
                environment[TELEMETRY_LEVEL] = telemetry
            if trace:
                staged, room = self.trace_stage(namespace)
                environment[TRACE_OUTPUT] = staged
                writes = tuple(writes) + (os.path.dirname(staged),)
            setup = None
            if entry["setup"] == "seed_copy":
                self.copy_seed(side, case, os.path.join(scratch, "seed"),
                               cli_seed=entry["seed"])
            elif entry["setup"] != "none":
                setup = (["/bin/sh", os.path.join(self.cli.fixtures, "setup.sh"),
                          entry["setup"], scratch, binary, probe or "-", "-"], environment,
                         scratch, CLI_SETUP_SECONDS)
            argv = [clicases.expand(text, values) for text in entry["argv"]]
            if wrap is not None:
                argv = wrap(argv)
            log = self.plan["limits"]["sample_log_bytes"]
            result = LAUNCHING
            result = self.tool_run(
                argv, case["sample_timeout_ms"] / 1000.0, log - log // 4,
                clicases.expand(entry["cwd"], values), env=environment, setup=setup,
                patterns=clicases.patterns(entry), selectors=clicases.selectors(entry),
                file_bytes=file_bytes, reads=(binary, probe, self.cli.fixtures),
                writes=(namespace.path,) + tuple(writes))
            if trace and not _uncertain(result):
                # A trace whose bytes left the evidence over its room is that reason whether the
                # watch cancelled the command while it was live or it finished first: settled
                # on the bytes held now, before any supervision failure the cancellation caused.
                self.settled_trace()
            failure = result.supervision_failure(observed_stream=True)
            if failure is not None:
                raise perfcapture.CaptureError("cli_failed", "%s: %s" % (what, failure))
            reply = result.supervised.result
            passed, why, _ = clicases.judge(entry, values, result.code, result.stdout,
                                            reply["stdout"]["truncated"],
                                            reply["stdout_pattern_counts"],
                                            reply["stdout_json"])
            if not passed:
                raise perfcapture.CaptureError("oracle_failed", "%s failed its oracle: %s"
                                               % (what, why))
            wall = format_decimal(Fraction(int(reply["timing"]["child_wall_ns"]), 1000000))
            exported = self.exported_trace(staged, room) if trace else None
            if after is not None:
                after(namespace, environment)
            return result, wall, exported
        finally:
            try:
                self.discard_trace(staged, LAUNCHING if getattr(
                    namespace, "reader_uncertain", False) else result)
            finally:
                self.release(namespace, _uncertain(result))

    def trace_stage(self, namespace):
        """(path, room) of the raw trace one traced execution exports: a new file below the
        evidence's `TRACE_STAGE` (the stage's when contained), so its bytes count against the
        evidence room while they are written and the step's watch cancels a trace that outgrows
        it; `room` is the evidence room left before it starts. Refused when none is left."""
        room = self.budget.room()
        if room < 1:
            raise Exhausted("profile_bytes_exhausted", "no evidence room is left for the "
                            "exported trace")
        return self.produced("%s/%s/trace.json" % (TRACE_STAGE, namespace.name)), room

    def exported_trace(self, staged, room):
        """The trace a perf-trace build exported at `staged` (`trace_stage`), admitted as a
        confined, bounded regular file before anything reads it (ADR-0175). The step is settled
        first: a trace that left the evidence over its room is refused with the bytes it holds
        (`discard_trace` keeps and counts them). An admitted trace is unstaged, its bytes charged
        again only when they are kept as evidence."""
        directory, name = os.path.split(staged)
        self.settled_trace()
        try:
            data = self.admitted(directory, name, min(TRACE_BYTES_MAX, room),
                                 "the exported trace")
        except FileNotFoundError:
            raise perfcapture.CaptureError("trace_missing", "the perf-trace build exported no "
                                           "trace")
        except Exhausted as error:
            raise perfcapture.CaptureError("trace_over_cap", error.detail)
        except ContractError as error:
            raise perfcapture.CaptureError("trace_unadmitted", str(error))
        self.unstage(directory)
        return data

    def settled_trace(self):
        """Settle a traced execution that ended with its supervisor's confirmation: evidence
        over its room is `trace_over_cap` with the bytes it holds now, the same reason and
        account for a command the watch cancelled while it was live and for one that finished
        before the watch looked (root p6a-integrity-root-trace-cap-reason)."""
        try:
            self.settle("the exported trace")
        except Exhausted as error:
            raise perfcapture.CaptureError("trace_over_cap", error.detail)
        except ContractError as error:
            raise perfcapture.CaptureError("trace_unadmitted", str(error))

    def discard_trace(self, staged, result):
        """After a traced execution that was not admitted: its staged trace is removed only
        when nothing may still write it and the evidence is within its room. A trace a live
        process may reach stays with its kept scratch; one that left the evidence over its room
        is kept and counted where it is (moved from a contained stage into the evidence, so the
        run's cleanup of its work does not erase it), never deleted to report nothing."""
        if staged is None or self.retained or _uncertain(result):
            return
        directory = os.path.dirname(staged)
        if not os.path.lexists(directory):
            return
        if self.over_room() is not None:
            if self.contained():
                self.collect(os.path.relpath(directory, self.stage))
            return
        self.unstage(directory)

    def unstage(self, directory):
        """Remove one execution's staged trace directory and the stage root once it is empty;
        a removal that is not confirmed is the run's cleanup failure."""
        self.halted("removing " + directory)
        problems = admission.remove_tree(directory) if os.path.lexists(directory) else []
        if problems:
            raise perfcapture.CaptureError("cleanup_uncertain", "; ".join(problems)[:4096])
        parent = os.path.dirname(directory)
        try:
            os.rmdir(parent)
        except OSError:
            pass

    def cli_traced(self, side, case, binary, trace):
        """One traced, or untraced, CLI execution; (None, wall ms, trace bytes or None)."""
        _, wall, exported = self.cli_run(side, case, binary, trace=trace,
                                         what="the traced command" if trace
                                         else "the untraced command")
        return None, wall, exported

    # Telemetry captures (ADR-0214) --------------------------------------------------------

    def telemetry_case(self, case):
        """One telemetry-recorded execution of an eligible CLI case per side, read back through
        that side's own `af telemetry` documents; every other case is an omission that names
        the supported matrix. Never a verdict sample, and never instead of the recipe's own
        evidence."""
        reason = telemetrydiag.eligibility(case)
        if reason is not None:
            self.omit("telemetry", case, None, reason[0], reason[1], reproduce=False)
            return
        if not self.cli_ready(case, "telemetry"):
            return
        if self.supervisor is None:
            self.omit("telemetry", case, None, "no_supervisor", "the build has no verified "
                      "authority supervisor to run anything under")
            return
        expected = None
        for rule in self.cli_entry(diagcontracts.SIDES[0], case)["rules"]:
            if rule["kind"] == "json_equals" and rule["path"] == "task_id":
                expected = rule["value"]
        for side in diagcontracts.SIDES:
            relative = "telemetry/%s/%s" % (case["instance_id"], side)
            binary, binary_sha256 = self.afs[side]
            entry = {"instance_id": case["instance_id"], "side": side,
                     "subject": "cli_process", "level": self.telemetry_level,
                     "source_sha": self.plan["resolved"][side + "_sha"],
                     "binary_sha256": binary_sha256, "command_wall_ms": None, "run": None,
                     "af": None, "executions": [], "evidence": None, "spans": 0, "phases": [],
                     "timeline": {"spans": [], "omitted": 0}, "artifacts": [],
                     "omission": None}
            documents = {}
            try:
                self.budget.check("a telemetry capture of " + case["instance_id"])
                self.directory(relative)

                def read_back(namespace, environment, binary=binary, relative=relative,
                              documents=documents):
                    self.telemetry_documents(binary, namespace, environment, relative,
                                             documents)

                _, wall, _ = self.cli_run(side, case, binary, telemetry=self.telemetry_level,
                                          after=read_back,
                                          what="the telemetry-recorded command")
                entry["command_wall_ms"] = wall
                derived = telemetrydiag.capture(documents, expected)
                self.write(relative + "/" + telemetrydiag.PERFETTO, telemetrydiag.perfetto(
                    documents["show"], "%s %s" % (case["instance_id"], side)))
                entry.update(derived)
            except (perfcapture.CaptureError, Exhausted) as error:
                entry["omission"] = {"reason": error.reason, "detail": error.detail[:4096]}
            except ContractError as error:
                entry["omission"] = {"reason": "telemetry_unreadable",
                                     "detail": str(error)[:4096]}
            except OSError as error:
                entry["omission"] = {"reason": "driver_error", "detail": str(error)[:4096]}
            if entry["omission"] is not None:
                entry.update(run=None, af=None, executions=[], evidence=None, spans=0,
                             phases=[], timeline={"spans": [], "omitted": 0})
            if os.path.isdir(os.path.join(self.output, relative)):
                try:
                    entry["artifacts"] = self.artifacts(relative)
                except (ContractError, OSError) as error:
                    entry["artifacts"] = []
                    if entry["omission"] is None:
                        entry["omission"] = {"reason": "telemetry_unadmitted",
                                             "detail": str(error)[:4096]}
                        entry.update(run=None, af=None, executions=[], evidence=None, spans=0,
                                     phases=[], timeline={"spans": [], "omitted": 0})
            self.telemetry.append(entry)

    def telemetry_documents(self, binary, namespace, environment, relative, documents):
        """Each of the side's own `af telemetry` readers in the recording's namespace, its stdout
        a new evidence file bounded while it is written and admitted before it is read; the
        bytes of each document, by kind, into `documents`. Show and config name the one run the
        runs document lists, and any reader that fails stops the capture."""
        run = None
        for kind in telemetrydiag.DOCUMENT_KINDS:
            if kind in ("show", "config") and run is None:
                runs = telemetrydiag.read("runs", documents["runs"], relative + "/runs.json")
                run = telemetrydiag.select_run(runs, None)
            target = "%s/%s.json" % (relative, kind)
            room = min(self.budget.room(), telemetrydiag.DOCUMENT_BYTES_MAX)
            if room < 1:
                raise Exhausted("profile_bytes_exhausted", "no evidence room is left for "
                                + target)
            path = self.produced(target)
            argv = telemetrydiag.read_argv(binary, kind, run)
            # The namespace owner outlives all readers. Mark the launch before calling
            # the supervisor: an exception after launch has no confirmed cleanup receipt.
            # A later result must never erase uncertainty from any earlier reader.
            prior = getattr(namespace, "reader_uncertain", False)
            if prior:
                raise perfcapture.CaptureError("cleanup_uncertain",
                                               "an earlier telemetry reader may still be live")
            result = LAUNCHING
            namespace.reader_uncertain = True
            try:
                result = self.tool_run(argv, telemetrydiag.READ_SECONDS, 4096, namespace.path,
                                       env=environment, stdout_path=path, file_bytes=room,
                                       reads=(binary,), writes=(namespace.path,))
            finally:
                namespace.reader_uncertain = prior or _uncertain(result)
            if namespace.reader_uncertain:
                raise perfcapture.CaptureError("cleanup_uncertain",
                                               "af telemetry %s cleanup is uncertain" % kind)
            if result.exceeded is not None:
                raise Exhausted(OVER_ROOM, "%s: %s" % (target, result.exceeded))
            if result.failure() is not None:
                raise perfcapture.CaptureError("telemetry_read_failed", "af telemetry %s: %s"
                                               % (kind, result.failure()))
            self.settle(target)
            self.collect(target)
            documents[kind] = self.admitted(self.output, target, room, target)

    def write_cli_seed(self, side, seed_id, case):
        """The side's CLI seed `seed_id`, written once by its own writer: `setup.sh` runs the
        seed's writer as the untimed setup of one supervised request whose child publishes what
        was written to the seed's new directory (ADR-0169). Its exit is not its admission: the
        whole tree is then enumerated as confined, bounded entries, sealed and identified."""
        spec = self.cli.doc["seeds"][seed_id]
        if side not in self.probes:
            raise perfcapture.CaptureError("seed_failed", "the %s side has no diagnostic probe "
                                           "to write its %s seed" % (side, seed_id))
        store = os.path.join(self.work, "seeds")
        os.makedirs(store, 0o700, exist_ok=True)
        path = os.path.join(store, "%s-cli-%s" % (side, seed_id))
        os.mkdir(path, 0o700)
        namespace = self.namespace()
        result = None
        try:
            scratch = namespace.path
            af, probe = self.afs[side][0], self.probes[side][0]
            environment = self.cli_environment(case, scratch)
            script = os.path.join(self.cli.fixtures, "setup.sh")
            request = canonical_bytes(clicases.seed_request(self.cli.doc, seed_id, scratch))
            result = LAUNCHING
            result = self.tool_run(
                ["/bin/sh", script, "seed_publish", scratch, af, probe, path],
                CLI_SETUP_SECONDS, CLI_SEED_LOG_BYTES, scratch, env=environment,
                setup=(["/bin/sh", script, spec["writer"], scratch, af, probe,
                        request.decode("utf-8")], environment, scratch, CLI_SETUP_SECONDS),
                reads=(af, probe, self.cli.fixtures), writes=(scratch, path))
        finally:
            self.release(namespace, _uncertain(result))
        if result.failure() is not None:
            raise perfcapture.CaptureError("seed_failed", "the %s side's %s seed writer "
                                           "failed: %s" % (side, seed_id, result.failure()))
        try:
            seeds.seal(path, self.seed_bounds(), self.deadlines())
            return path, seeds.tree(path, self.seed_bounds(), self.deadlines())[0]
        except deadlines.Expired as expired:
            raise perfcapture.CaptureError("diagnostic_deadline", str(expired))
        except (ContractError, OSError) as error:
            raise perfcapture.CaptureError("seed_unadmitted", "the %s side's written %s seed "
                                           "is not admitted: %s" % (side, seed_id, error))

    # Phase traces ------------------------------------------------------------------------

    def run_probe(self, side, case, binary, trace_path=None):
        """One unbracketed fresh repetition; (judged, wall_ms decimal, trace bytes or None).
        The trace the probe exported is written to its stage, counted with the evidence while
        it is written, and admitted as a confined bounded regular file before anything reads
        it."""
        namespace = self.namespace()
        result = staged = room = None
        try:
            request, request_path = self.prepared(side, case, namespace)
            env = supervise.probe_environment(namespace.path)
            writes = (namespace.path,)
            if trace_path:
                staged, room = self.trace_stage(namespace)
                env[TRACE_OUTPUT] = staged
                writes += (os.path.dirname(staged),)
            result = LAUNCHING
            result = self.tool_run([binary], case["sample_timeout_ms"] / 1000.0,
                                   limits.RECORD_BYTES_MAX, namespace.path, env=env,
                                   stdin=canonical_bytes(request),
                                   file_bytes=room, writes=writes)
            if result.supervision_failure() is not None:
                # Before any oracle: a truncated stdout whose prefix parses, a refused
                # lifecycle or a cancellation never becomes a passing probe (ADR-0176).
                raise perfcapture.CaptureError("probe_failed", result.supervision_failure())
            judged = self.judged(request, result.code, result.stdout, "the probe")
            trace = self.exported_trace(staged, room) if trace_path else None
            return judged, judged["metrics"]["wall_ms"]["value"], trace
        finally:
            try:
                self.discard_trace(staged, result)
            finally:
                self.release(namespace, _uncertain(result))

    def trace_case(self, case):
        """Phase traces of both sides and their trace-on/off overhead against the verdict
        build: a native probe's, whose region is `probe_region`, or a CLI process's, whose
        region is its startup or its dispatched command. Each trace states how much of its
        region it holds: a recorder that its setup filled leaves no region span, and that trace
        is labelled `region_missing`, never complete (ADR-0176)."""
        pairs = self.registry["trace"]["overhead_pairs"]
        target = self.registry["trace"]["overhead_target"]
        if case["kind"] == "library":
            binaries, parents, execute = self.probes, self.parent_probes, self.run_probe
            subject, phase = "probe_region", "probe_region"
        else:
            binaries, parents, execute = self.afs, self.parent_afs, self.cli_traced
            subject, phase = "cli_process", cli_region_phase(case)
        for side in diagcontracts.SIDES:
            relative = "traces/%s/%s" % (case["instance_id"], side)
            entry = {"instance_id": case["instance_id"], "side": side, "subject": subject,
                     "region_phase": phase, "region_spans": 0, "coverage": None, "spans": 0,
                     "dropped": 0, "open": 0, "async_spans": 0, "overhead": None,
                     "artifacts": [], "omission": None}
            off, on, kept = [], [], None
            parent = parents.get(side)
            try:
                if parent is None:
                    self.budget.check("a trace of " + case["instance_id"])
                    _, value, kept = execute(side, case, binaries[side][0], True)
                    self.omit("trace_overhead", case, side, "no_uninstrumented_build",
                              "no verdict build was given (--parent-build), so trace-on/off "
                              "overhead was not measured")
                else:
                    for index in range(pairs):
                        order = ("off", "on") if index % 2 == 0 else ("on", "off")
                        for kind in order:
                            self.budget.check("overhead pair %d of %s %s"
                                              % (index + 1, case["instance_id"], side))
                            binary = parent[0] if kind == "off" else binaries[side][0]
                            _, value, trace = execute(side, case, binary, kind == "on")
                            (off if kind == "off" else on).append(value)
                            if trace is not None and kept is None:
                                kept = trace
                doc = phasetrace.load(kept, relative + "/trace.json")
                self.directory(relative)
                perfetto = phasetrace.perfetto(doc, "%s %s" % (case["instance_id"], side))
                self.write(relative + "/trace.json", kept)
                self.write(relative + "/perfetto.json", perfetto)
                region_spans, coverage = trace_coverage(doc, phase)
                entry.update(spans=len(doc["spans"]), dropped=doc["dropped"],
                             open=doc["open"], async_spans=phasetrace.async_count(doc),
                             region_spans=region_spans, coverage=coverage)
                if off:
                    overhead = diagcontracts.trace_overhead(off, on)
                    entry["overhead"] = {
                        "pairs": pairs,
                        "off_recipe_digest": self.parent_plan["profile"]["recipe_digest"],
                        "on_recipe_digest": self.plan["profile"]["recipe_digest"],
                        "off_binary_sha256": parent[1],
                        "on_binary_sha256": binaries[side][1],
                        "off_ms": off, "on_ms": on,
                        "median_overhead": format_rational(overhead), "target": target,
                        "label": ("perturbing" if overhead > Fraction(target)
                                  else "within_target")}
                entry["artifacts"] = self.artifacts(relative)
                self.trace_docs.append(doc)
            except (perfcapture.CaptureError, Exhausted) as error:
                entry["omission"] = {"reason": error.reason, "detail": error.detail[:4096]}
            except ContractError as error:
                entry["omission"] = {"reason": "trace_refused", "detail": str(error)[:4096]}
            except OSError as error:
                entry["omission"] = {"reason": "driver_error", "detail": str(error)[:4096]}
            if entry["omission"] is not None:
                entry.update(spans=0, dropped=0, open=0, async_spans=0, overhead=None,
                             artifacts=[], region_spans=0, coverage=None)
                self.trace_docs.append(None)
            self.traces.append(entry)

    # Counters ----------------------------------------------------------------------------

    def counter_case(self, case):
        if case["instance_id"] not in self.registry["counters"]["cases"]:
            self.omit("counter", case, None, "not_a_counter_case", "counters are declared for "
                      "small library cases only (diagnostics.toml counters.cases)",
                      reproduce=False)
            return
        valgrind, control = self.tool("valgrind"), self.tool("callgrind_control")
        for side in diagcontracts.SIDES:
            relative = "counters/%s/%s" % (case["instance_id"], side)
            directory = self.directory(relative)
            base = {"instance_id": case["instance_id"], "side": side,
                    "interpretation": "explanatory_proxy_not_wall_time", "artifacts": [],
                    "omission": None, "value": None, "duration_ms": None}
            callgrind = dict(base, tool="callgrind", metric="instructions", scope="region")
            output = self.produced(relative + "/callgrind.out")
            try:
                if valgrind is None or control is None:
                    raise perfcapture.CaptureError("valgrind_unavailable", "valgrind and "
                                                   "callgrind_control are not configured")
                if self.contained():
                    # Its region switch is callgrind_control signalling the profiled process,
                    # which another container's PID namespace cannot reach.
                    raise perfcapture.CaptureError(
                        "callgrind_switch_not_contained", "callgrind_control switches the "
                        "region of a process in its own PID namespace; each contained request "
                        "has its own, so Callgrind's region count runs only in trusted local "
                        "mode")
                self.budget.check("Callgrind of " + case["instance_id"])
                switch = dict((state, counters.callgrind_switch(
                    self.supervisor, control, TOOL_ENV, state, self.budget.tool_seconds,
                    directory, os.path.basename(self.probes[side][0])))
                    for state in ("on", "off"))
                run, _ = self.bracketed(
                    side, case,
                    lambda fifo: counters.callgrind_argv(valgrind, output,
                                                         [self.probes[side][0]]),
                    switch["on"], switch["off"], self.registry["counters"]["seconds"],
                    repeat=False, file_bytes=self.budget.room(),
                    writes=(os.path.dirname(output),))
                region = run.regions[0]
                self.settle("Callgrind of " + case["instance_id"])
                self.collect(relative + "/callgrind.out")
                text = self.admitted(directory, "callgrind.out",
                                     min(SCRIPT_BYTES_MAX, self.budget.evidence_limit),
                                     relative + "/callgrind.out")
                callgrind.update(value=counters.parse_callgrind(text.decode("utf-8",
                                                                           "replace")),
                                 duration_ms=_ms(region.disabled - region.enabled))
                callgrind["artifacts"] = self.artifacts(relative)
            except (perfcapture.CaptureError, counters.CounterError, Exhausted) as error:
                callgrind["omission"] = {"reason": error.reason, "detail": error.detail[:4096]}
            except (ContractError, OSError) as error:
                callgrind["omission"] = {"reason": "callgrind_failed", "detail": str(error)}
            if callgrind["omission"] is not None:
                callgrind.update(value=None, duration_ms=None, artifacts=[])
            self.counters.append(callgrind)
            for metric in ("allocations", "allocated_bytes"):
                self.counters.append(dict(base, tool="dhat", metric=metric,
                                          scope="whole_process"))
            dhat = self.counters[-2:]
            try:
                if valgrind is None:
                    raise perfcapture.CaptureError("valgrind_unavailable",
                                                   "valgrind is not configured")
                self.budget.check("DHAT of " + case["instance_id"])
                namespace = self.namespace()
                result = None
                try:
                    request, request_path = self.prepared(side, case, namespace)
                    out = self.produced(relative + "/dhat.json")
                    started = self.clock()
                    result = LAUNCHING
                    result = self.tool_run(counters.dhat_argv(valgrind, out,
                                                              [self.probes[side][0]]),
                                           self.registry["counters"]["seconds"],
                                           limits.RECORD_BYTES_MAX, namespace.path,
                                           env=supervise.probe_environment(namespace.path),
                                           stdin=canonical_bytes(request),
                                           file_bytes=self.budget.room(),
                                           writes=(namespace.path, os.path.dirname(out)))
                    elapsed = self.clock() - started
                    if result.supervision_failure() is not None:
                        raise perfcapture.CaptureError("dhat_failed",
                                                       result.supervision_failure())
                    self.judged(request, result.code, result.stdout, "the DHAT probe")
                finally:
                    self.release(namespace, _uncertain(result))
                self.settle("DHAT of " + case["instance_id"])
                self.collect(relative + "/dhat.json")
                allocations, allocated = counters.parse_dhat(
                    result.stderr.decode("utf-8", "replace"))
                self.write(relative + "/dhat.stderr", result.stderr)
                for record, value in zip(dhat, (allocations, allocated)):
                    record.update(value=value, duration_ms=_ms(elapsed))
                dhat[0]["artifacts"] = [item for item in self.artifacts(relative)
                                        if item["path"].rsplit("/", 1)[-1]
                                        in ("dhat.json", "dhat.stderr")]
            except (perfcapture.CaptureError, counters.CounterError, Exhausted) as error:
                for record in dhat:
                    record["omission"] = {"reason": error.reason, "detail": error.detail[:4096]}
            except (ContractError, OSError) as error:
                for record in dhat:
                    record["omission"] = {"reason": "dhat_failed", "detail": str(error)[:4096]}

    # The run -----------------------------------------------------------------------------

    def run(self):
        kind = self.recipe["kind"]
        for case in self.cases:
            self.case_run(case, kind)
            if self.telemetry_level is not None:
                self.telemetry_case(case)
        for work in (self.work, os.path.join(self.output, WORK_DIR)):
            if os.path.isdir(work) and not self.retained:
                # Only this run's own work directories: its seeds, FIFOs, staged outputs and
                # confirmed-empty scratch, and the authority's materialized fixtures.
                problems = admission.remove_tree(work)
                if problems:
                    self.retained.append(work)
        return self.document()

    def case_run(self, case, kind):
        """The recipe kind's evidence of one selected case, or its omissions."""
        if case["kind"] != "library" and not self.cli_ready(case, kind):
            return
        missing = [side for side in diagcontracts.SIDES if side not in self.probes]
        if missing and case["kind"] == "library":
            self.omit("build", case, None, "no_diagnostic_build", "the %s side has no "
                      "diagnostic probe binary" % ", ".join(missing))
            return
        if self.supervisor is None:
            self.omit(OMISSION_KIND[kind], case, None, "no_supervisor", "the build has no "
                      "verified authority supervisor to run anything under")
            return
        try:
            self.budget.check(case["instance_id"])
        except Exhausted as error:
            self.omit(OMISSION_KIND[kind], case, None, error.reason, error.detail)
            return
        if kind == "cpu_profile" and "samply" in self.recipe["tools"]:
            if self.tool("samply") is None:
                self.omit("samply", case, None, "samply_unavailable",
                          self.tools["samply"][0]["detail"])
            else:
                self.samply_case(case)
        elif kind == "cpu_profile":
            if self.tool("perf") is None:
                self.omit("cpu_profile", case, None, "perf_unavailable",
                          self.tools["perf"][0]["detail"])
            else:
                self.perf_case(case)
        elif kind == "trace":
            self.trace_case(case)
        else:
            self.counter_case(case)

    def cli_ready(self, case, kind):
        """Whether a CLI case can be diagnosed here; otherwise its omission is recorded with
        the host capability or input it lacks, never as an implementation that is missing."""
        if kind == "counters":
            # Counters are declared for small library cases only (counters.cases).
            return True
        if case["requires_linux_containment"] and not self.contained():
            self.omit(OMISSION_KIND[kind], case, None, "requires_linux_containment",
                      "%s spawns processes and is diagnosed only in its contained Linux "
                      "environment (--mode linux_container), never on this host's process "
                      "group" % case["instance_id"])
            return False
        if self.cli is None:
            self.omit(OMISSION_KIND[kind], case, None, "no_cli_registry", "the plan's "
                      "authority has no CLI registry for its process cases")
            return False
        absent = [side for side in diagcontracts.SIDES if side not in self.cli.adapters]
        if absent:
            self.omit(OMISSION_KIND[kind], case, None, "no_cli_adapter", "no CLI adapter covers "
                      "the %s side's package version" % ", ".join(absent))
            return False
        missing = [side for side in diagcontracts.SIDES if side not in self.afs]
        if missing:
            self.omit("build", case, None, "no_diagnostic_build", "the %s side has no "
                      "diagnostic AF binary" % ", ".join(missing))
            return False
        return True

    def document(self):
        recipe, frozen = self.recipe, self.plan["diagnostic"]["recipe"]
        doc = {
            "schema": diagcontracts.PROFILES_SCHEMA, "run_id": self.plan["run_id"],
            "parent_run_id": self.parent_plan["run_id"],
            "parent_plan_digest": self.parent_plan["plan_digest"],
            "plan_digest": self.plan["plan_digest"], "registry_sha256": self.registry_sha256,
            "verdict_effect": "none", "selection": self.selection,
            "sources": {"baseline_sha": self.plan["resolved"]["baseline_sha"],
                        "candidate_sha": self.plan["resolved"]["candidate_sha"],
                        "authority_sha": self.plan["resolved"]["authority_sha"]},
            "recipe": {"recipe_id": self.recipe_id, "kind": frozen["kind"],
                       "recipe_digest": self.plan["profile"]["recipe_digest"],
                       "base_recipe": frozen["base_recipe"], "features": recipe["features"],
                       "debug_symbols": recipe["debug_symbols"],
                       "frame_pointers": recipe["frame_pointers"],
                       "builds_sha256": self.builds_sha256},
            "capture": {"frequency_hz": self.frequency, "duration_ms": self.duration_ms,
                        "event": self.event,
                        "dwarf_stack_bytes": limits.DIAGNOSTIC_DWARF_STACK_BYTES},
            "budgets": self.budget.record(len(self.cases)),
            "execution": dict(self.execution, **{
                "started_unix_ms": self.budget.started_unix_ms,
                "ended_unix_ms": max(self.budget.started_unix_ms,
                                     int(self.budget.wall() * 1000)),
                "deadline_unix_ms": self.budget.deadline_unix_ms,
                "host": (socket.gethostname() or "unknown")[:256],
                "retained_scratch": len(self.retained)}),
            "tools": [entry[0] for _, entry in sorted(self.tools.items())],
            "captures": self.captures, "renders": self.renders, "traces": self.traces,
            "counters": self.counters,
            "telemetry": {"level": self.telemetry_level,
                          "perturbing": self.telemetry_level == "content",
                          "verdict_effect": "none", "captures": self.telemetry},
            "omissions": self.omissions[:256],
        }
        return diagcontracts.validate_profiles(doc)


class Renderer(object):
    """inferno's tools for one child run: each runs under the supervisor, writes its output as
    a new evidence file bounded by the room left, and that file is admitted before it is read;
    the sanitized copies are charged before they are written."""

    def __init__(self, profile, paths):
        self.profile = profile
        self.paths = paths

    def run(self, name, argv, relative):
        cwd = os.path.join(self.profile.output, os.path.dirname(relative))
        return self.profile.tool_output(relative, [self.paths[name]] + argv,
                                        "%s of %s" % (name.replace("_", "-"), relative),
                                        limit=self.profile.budget.tool_output_bytes)

    def read(self, relative):
        return self.profile.admitted(self.profile.output, relative,
                                     self.profile.budget.evidence_limit, relative)

    def write(self, relative, data):
        return self.profile.write(relative, data)

    def relative_to(self, source, target):
        """The admitted evidence file `source` as a tool writing `target` names it: its absolute
        evidence path, the same whether the tool writes the evidence or a contained stage."""
        return os.path.join(self.profile.output, source)


def _losses(losses):
    """A capture's loss records as the page shows them, the two payloads kept apart."""
    if losses["state"] != "decoded":
        return "unknown: " + losses["detail"]
    return ("%d lost events in %d LOST records; %d potentially lost samples in %d "
            "LOST_SAMPLES records" % (losses["lost_events"], losses["lost_event_records"],
                                      losses["potentially_lost_samples"],
                                      losses["lost_sample_records"]))


def side_of(record):
    return record["side"]


# The page ----------------------------------------------------------------------------------

def page(doc, traces):
    """The child run's offline page: identity, selection, budgets, tools, captures, sanitized
    flamegraphs as local images, phase timelines and counters, and every omission."""
    sections = [render.section("Diagnostic child run", render.paragraph(
        "Child run %s of comparison %s. Diagnostics never change the parent's verdict "
        "(verdict_effect: none); color in a difference flamegraph is the change in each "
        "stack's share of CPU samples, not an absolute time saved." % (
            doc["run_id"], doc["parent_run_id"])), render.table(
        ("field", "value"),
        [("recipe", "%s (%s, base %s, features %s)" % (
            doc["recipe"]["recipe_id"], doc["recipe"]["kind"], doc["recipe"]["base_recipe"],
            ", ".join(doc["recipe"]["features"]) or "none")),
         ("capture", "%d Hz for %d ms of region per side, event %s" % (
             doc["capture"]["frequency_hz"], doc["capture"]["duration_ms"],
             doc["capture"]["event"])),
         ("selection", "%s: %s" % (doc["selection"]["mode"], ", ".join(
             "%s (%s)" % (item["instance_id"], item["reason"])
             for item in doc["selection"]["cases"]) or "none"))]
        + [(name, "%d of %d" % (use["used"], use["limit"]))
           for name, use in sorted(doc["budgets"].items()) if name != "bytes"]
        + [("bytes", "%d evidence bytes of %d (an administrative reserve of %d kept within); "
            "overage %d" % (doc["budgets"]["bytes"]["evidence_used"],
                            doc["budgets"]["bytes"]["limit"],
                            doc["budgets"]["bytes"]["administrative_reserve"],
                            doc["budgets"]["bytes"]["evidence_overage"])),
           ("execution", "%s (%s), the plan's environment declares %s%s" % (
               doc["execution"]["mode"], doc["execution"]["containment"],
               doc["execution"]["declared_containment"],
               "" if doc["execution"]["matches_environment"] else "; not the same"))]))]
    sections.append(render.section("Tools", render.table(
        ("tool", "state", "version", "provenance", "sha256", "detail"),
        [(tool["name"], tool["state"], tool["reported_version"] or tool["declared_version"]
          or "", tool["version_provenance"], tool["executable_sha256"] or "",
          tool["detail"] or tool["provisioning"]) for tool in doc["tools"]], searchable=True)))
    sections.append(render.section("CPU captures", render.table(
        ("case", "side", "attempt", "unwind", "region", "samples", "lost", "unknown frames",
         "effective Hz", "symbols", "omission"),
        [(item["instance_id"], item["side"], item["attempt"], item["unwind"],
          "%s: %s ms of region (recording enabled %s ms) in %s ms, setup %s ms, %d "
          "repetitions, stopped: %s%s" % (
              item["region"]["mechanism"], item["region"]["region_ms"],
              item["region"]["enabled_ms"], item["region"]["wall_ms"],
              item["region"]["setup_ms"], item["region"]["repetitions"],
              item["region"]["stopped"],
              ", truncated at the cap" if item["region"]["truncated_at_cap"] else ""),
          item["samples"], _losses(item["losses"]), item["unknown_frame_ratio"],
          item["effective_frequency_hz"], item["symbols"]["state"],
          (item["omission"] or {}).get("reason", "")) for item in doc["captures"]],
        searchable=True)))
    graphs = []
    for item in doc["renders"]:
        # Only the sanitized SVG is a view. inferno's original may hold active markup, and an
        # SVG opened on its own is outside this page's CSP, so it is archived evidence named by
        # path and hash, never linked (ADR-0179).
        graphs.append("<h3>%s: %s</h3>%s<p>%s; inferno's original is archived unlinked as "
                      "%s (%s)</p>" % (
                          render.escape(item["instance_id"]), render.escape(item["kind"]),
                          render.image(item["sanitized"]["path"], "%s %s" % (
                              item["instance_id"], item["kind"])),
                          render.link(item["sanitized"]["path"], "sanitized SVG"),
                          render.code(item["original"]["path"]).text,
                          render.code(item["original"]["sha256"]).text))
    sections.append(render.section("Flamegraphs", "".join(graphs) or render.paragraph(
        "No flamegraph was rendered; see the omissions.", "unavailable")))
    timelines = []
    for item, trace in zip(doc["traces"], traces):
        if trace is None:
            continue
        overhead = item["overhead"]
        coverage = {"complete": "complete", "partial": "partial: spans were dropped or left "
                    "open", "region_missing": "region missing: no %s span was kept, so this "
                    "is not a trace of its region" % item["region_phase"]}[item["coverage"]]
        timelines.append("<h3>%s %s</h3>%s%s%s" % (
            render.escape(item["instance_id"]), render.escape(item["side"]),
            render.paragraph("%s, region %s (%d spans): %s; %d spans, %d dropped, %d open, %d "
                             "on async tracks; overhead %s"
                             % (item["subject"], item["region_phase"], item["region_spans"],
                                coverage, item["spans"], item["dropped"], item["open"],
                                item["async_spans"],
                                "%s (%s, target %s)" % (overhead["median_overhead"],
                                                        overhead["label"], overhead["target"])
                                if overhead else "not measured")),
            render.table(("phase", "spans", "covered ns (union)", "longest ns"),
                         [(row["phase"], row["spans"], row["covered_ns"], row["longest_ns"])
                          for row in phasetrace.phase_summary(trace)]),
            phasetrace.timeline_svg(trace).text))
    sections.append(render.section("Phase timelines", "".join(timelines) or render.paragraph(
        "No trace was captured; see the omissions.", "unavailable")))
    sections.append(render.section("Counters", render.table(
        ("case", "side", "tool", "metric", "scope", "value", "interpretation", "omission"),
        [(item["instance_id"], item["side"], item["tool"], item["metric"], item["scope"],
          item["value"], item["interpretation"], (item["omission"] or {}).get("reason", ""))
         for item in doc["counters"]])))
    sections.append(telemetrydiag.section(doc["telemetry"]))
    sections.append(render.section("Omissions", render.table(
        ("kind", "case", "side", "reason", "detail", "reproduction"),
        [(item["kind"], item["instance_id"] or "", item["side"] or "", item["reason"],
          item["detail"], " ".join(item["reproduction"] or ())) for item in doc["omissions"]],
        searchable=True)))
    return render.page("Diagnostics %s" % doc["run_id"], sections)


def finish(output, doc, traces, budget=None):
    """Write profiles.json, the page and the bundle manifest of a finished child run: the
    administrative record, held to the reserve its budget declares (ADR-0175)."""
    data = contracts.write_bytes(doc, "profiles")
    html = page(doc, traces)
    if budget is not None:
        require(len(data) + len(html) <= budget.reserve - limits.RECORD_BYTES_MAX,
                "the administrative record (%d bytes) does not fit its %d-byte reserve"
                % (len(data) + len(html), budget.reserve))
    with open(os.path.join(output, PROFILES_FILE), "xb") as stream:
        stream.write(data)
    with open(os.path.join(output, PAGE_FILE), "xb") as stream:
        stream.write(html)
    if doc["execution"]["retained_scratch"]:
        # The administrative refusal remains readable. A bundle manifest would read and
        # admit files a possibly live local tool can still write (local output is staging).
        return None
    limit = budget.bytes_limit if budget is not None else limits.DIAGNOSTIC_BYTES_MAX
    return bundle.write_manifest(output, limit, skip=(WORK_DIR,))


# The command -------------------------------------------------------------------------------

class Unavailable(ContractError):
    def __init__(self, reason, detail):
        ContractError.__init__(self, detail)
        self.reason = reason


class Options(object):
    def __init__(self, **values):
        self.plan = None
        self.verdict = None
        self.cases = []
        self.max_cases = None
        self.recipe = None
        self.output = None
        self.repo = "."
        self.build = None
        self.parent_build = None
        self.perf = None
        self.inferno_bin = None
        self.samply = None
        self.valgrind_bin = None
        self.duration_seconds = None
        self.frequency_hz = None
        self.total_minutes = None
        self.run_id = None
        # The explicitly selected execution mode; nothing is implied by a recipe (ADR-0175).
        self.mode = None
        # The contained Linux mode's container runtime, sample image pinned by digest, dedicated
        # disk-bounded scratch filesystem and container user (ADR-0176).
        self.docker = "docker"
        self.image = None
        self.scratch_fs = None
        self.uid = os.getuid()
        self.gid = os.getgid()
        # The machine lock's directory (tests only); the system's /tmp otherwise.
        self.lock_directory = None
        # How the contained host is checked (tests only): a callable (docker, image, scratch_fs,
        # environment, budget, clock) -> (scratch root, CPUs, runtime identity).
        self.contained_host = None
        # How to build when no --build is given: a callable (child plan, deadline unix ms) ->
        # build directory, or None to record the build as unavailable.
        self.builder = None
        # The build inputs a builder was given, named again by a reproduction command
        # (`reproduce.profile`): host paths as placeholders, the image reference as given.
        self.toolchain_bin = None
        self.cargo_home = None
        self.zigbuild_bin = None
        self.build_image = None
        self.host_os = None
        # The explicitly selected telemetry level of each eligible CLI case's telemetry capture
        # (`spans` or `content`), or None for none (ADR-0214).
        self.telemetry = None
        for key, value in values.items():
            require(hasattr(self, key), "unknown profile option " + key)
            setattr(self, key, value)


def _read(path, name):
    with open(path, "rb") as stream:
        data = stream.read(limits.MAX_JSON_BYTES + 1)
    return contracts.read_json(data, name, path), data


def _probes(build_dir, plan, deadline, afs=None):
    """({side: (path, sha256)}, supervisor (path, sha256) or None, the build manifest's
    SHA-256) of a verified build of `plan`, every artifact rehashed before the monotonic
    `deadline`; with `afs`, each side's AF binary is added to it, by side."""
    manifest = builds.read_manifest(os.path.abspath(build_dir))
    require(manifest[0]["run_id"] == plan["run_id"]
            and manifest[0]["plan_digest"] == plan["plan_digest"],
            "the build at %s is of another run or plan" % build_dir)
    doc, paths = builds.verify_artifacts(os.path.abspath(build_dir),
                                         deadlines.Budget(deadline, time.monotonic, 0), manifest)
    out = {}
    for side in diagcontracts.SIDES:
        artifact = doc["sides"][side]["artifacts"]["probe"]
        if artifact is not None:
            out[side] = (paths[side + "/probe"], artifact["sha256"])
        built = doc["sides"][side]["artifacts"]["af"]
        if afs is not None and built is not None:
            afs[side] = (paths[side + "/af"], built["sha256"])
    supervisor = None
    if doc["supervisor"]["status"] == "succeeded" and "supervisor" in paths:
        supervisor = (paths["supervisor"], doc["supervisor"]["artifact"]["sha256"])
    return out, supervisor, sha256_bytes(manifest[1])


def host_os():
    import sys
    return "macos" if sys.platform == "darwin" else "linux"


def check_mode(plan, mode, host=None):
    """The explicitly selected execution mode and whether it is the one the plan's environment
    declares (ADR-0175, ADR-0176). Explicitly trusted local mode runs everything on this host
    under the supervisor's process-group containment and is recorded as such, never as
    contained. The contained Linux mode runs each execution in its own sample container, and
    only for an environment that declares it, on a Linux host; nothing else substitutes for
    it."""
    environment = plan["profile"]["environment"]
    require(mode in MODES, "--mode: %s, selected explicitly; nothing is implied by a "
            "diagnostic recipe" % " or ".join(MODES))
    declared = environment["containment"]
    if mode == LINUX_CONTAINER:
        if declared != "linux_container":
            raise Unavailable("mode_mismatch", "environment %s runs as %s; --mode %s is not a "
                              "substitute" % (plan["profile"]["environment_id"], declared,
                                              mode))
        if (host or host_os()) != "linux":
            raise Unavailable("host_mismatch", "environment %s is the contained Linux profile; "
                              "this host is %s" % (plan["profile"]["environment_id"],
                                                   host or host_os()))
    return mode, declared


def run_profile(git, options, authority_factory=None):
    """Create one diagnostic child run; (exit code, profiles path or None, message)."""
    from . import planning, runner
    # Every explicit bound is checked before anything is created, built or launched.
    frequency = explicit(options.frequency_hz, "--frequency-hz", 1, limits.DIAGNOSTIC_HZ_MAX)
    duration = explicit(options.duration_seconds, "--duration-seconds", 1,
                        limits.DIAGNOSTIC_SECONDS_MAX)
    explicit(options.total_minutes, "--total-minutes", 1, limits.DIAGNOSTIC_RUN_MINUTES_MAX)
    explicit(options.max_cases, "--max-cases", 1, limits.DIAGNOSTIC_CASES_MAX)
    require(options.telemetry in (None,) + telemetrydiag.LEVELS,
            "--telemetry: one of " + ", ".join(telemetrydiag.LEVELS))
    parent, _ = _read(options.plan, "run")
    require(parent["scope"] != "diagnostic", "--plan: a diagnostic's parent is a comparison")
    mode, declared = check_mode(parent, options.mode, options.host_os)
    authority_sha = parent["resolved"]["authority_sha"]
    suite = planning.read_suite(git, authority_sha)
    raw = git.read_file(authority_sha, REGISTRY_PATH)
    if raw is None:
        raise Unavailable("no_diagnostic_registry", "the authority %s has no %s; its plans "
                          "have no diagnostic recipes" % (authority_sha[:12], REGISTRY_PATH))
    registry = load_bytes(raw, REGISTRY_PATH, suite.doc)
    require(options.recipe in registry["recipes"], "--recipe: one of " + ", ".join(
        sorted(registry["recipes"])))
    entry = registry["recipes"][options.recipe]
    require(options.telemetry is None or entry["kind"] in ("cpu_profile", "trace"),
            "--telemetry: captured beside CPU profiles or phase traces; recipe %s is a %s "
            "recipe" % (options.recipe, entry["kind"]))
    base = suite.doc["recipes"][entry["base_recipe"]]
    require(base["target"] == parent["profile"]["recipe"]["target"]
            and base["os"] == parent["profile"]["recipe"]["os"],
            "--recipe %s is built for %s, not the parent's %s" % (
                options.recipe, base["target"], parent["profile"]["recipe"]["target"]))
    verdict = verdict_sha = None
    if options.verdict:
        verdict, data = _read(options.verdict, "verdict")
        verdict_sha = sha256_bytes(data)
    selection, cases = select(parent, verdict, options.cases, options.max_cases, verdict_sha)
    if not cases:
        return 0, None, ("nothing to diagnose: the parent verdict has no regressed or "
                         "inconclusive case")
    frequency = registry["capture"]["frequency_hz"] if frequency is None else frequency
    duration = registry["capture"]["duration_seconds"] if duration is None else duration
    request = planning.Request(scope="diagnostic", parent_plan=parent,
                               cases=[case["instance_id"] for case in cases],
                               run_id=options.run_id,
                               diagnostic_duration_ms=duration * 1000,
                               diagnostic_frequency_hz=frequency)
    plan = planning._diagnostic(git, request, recipe=(
        options.recipe, diagcontracts.diagnostic_recipe(suite.doc, registry, options.recipe),
        diagcontracts.frozen_identity(registry, options.recipe)))
    require(not os.path.lexists(options.output), "--output %s already exists; a child run "
            "writes a new directory" % options.output)
    # The machine's one coordination lock, the one timed shards take (ADR-0169), from before
    # the child run's directory exists until its terminal evidence is written.
    try:
        lock = runner.machine_lock(options.lock_directory)
    except runner.Unavailable as refused:
        raise Unavailable(refused.reason, str(refused))
    try:
        return _run_locked(git, options, authority_factory, parent, plan, registry, raw, entry,
                           selection, cases, frequency, mode, declared, lock.path)
    finally:
        lock.release()


def _contained(options, plan, budget):
    """The contained Linux host of a child run, checked under the machine lock before its
    directory exists (ADR-0176): the same preflight a shard takes (`runner.contained_host`), then
    the run's own new directory on the verified scratch filesystem. Unavailable with its reason
    otherwise; nothing changes a host setting or privilege."""
    from . import runner
    environment = plan["profile"]["environment"]
    require(options.uid == os.geteuid(), "--mode linux_container runs its containers as this "
            "user (uid %d), so the run can verify and remove what they wrote" % os.geteuid())
    check = options.contained_host or runner.contained_host
    try:
        scratch_root, cpus, runtime = check(options.docker, options.image, options.scratch_fs,
                                            environment, deadlines.Budget(budget.deadline,
                                                                          budget.clock, 0),
                                            budget.clock)
    except runner.Unavailable as refused:
        raise Unavailable(refused.reason, str(refused))
    except deadlines.Expired as expired:
        raise Unavailable("diagnostic_deadline", str(expired))
    scratch_root = os.path.realpath(scratch_root)
    work = os.path.join(scratch_root, supervise.DIAGNOSTIC_DIRECTORY)
    if os.path.lexists(work):
        raise Unavailable("scratch_in_use", "%s is still on the scratch filesystem, kept by an "
                          "earlier child run whose cleanup was not confirmed; the operator "
                          "removes it" % work)
    return {"scratch_root": scratch_root, "cpus": cpus, "runtime": runtime, "work": work,
            "environment": environment}


def _run_locked(git, options, authority_factory, parent, plan, registry, raw, entry, selection,
                cases, frequency, mode, declared, lock_path):
    from . import runner
    output = os.path.abspath(options.output)
    budget = Budget(registry["limits"], output, total_minutes=options.total_minutes)
    host = options.host_os or host_os()
    contained = _contained(options, plan, budget) if mode == LINUX_CONTAINER else None
    os.makedirs(output, 0o700)
    with open(os.path.join(output, PLAN_FILE), "xb") as stream:
        stream.write(contracts.write_bytes(plan, "run"))
    omissions = []

    def reproduction(instance_id):
        # The whole selection, capture bounds, recipe, mode and build inputs as this run had
        # them (ADR-0177); one case's command names only that case.
        return reproduce.profile(options, plan, entry["tools"], mode, selection, instance_id)

    builds_sha = None
    probes, afs = {}, {}
    supervisor = None
    try:
        if options.build:
            probes, supervisor, builds_sha = _probes(options.build, plan, budget.deadline, afs)
        elif options.builder is not None:
            budget.build(1 if plan["resolved"]["baseline_sha"]
                         == plan["resolved"]["candidate_sha"] else 2)
            built = options.builder(plan, budget.deadline_unix_ms)
            probes, supervisor, builds_sha = _probes(built, plan, budget.deadline, afs)
        else:
            omissions.append({"kind": "build", "instance_id": None, "side": None,
                              "reason": "no_diagnostic_build",
                              "detail": "neither --build nor a build configuration was given",
                              "reproduction": reproduction(None)})
    except (Exhausted, Unavailable, builds.Unavailable, deadlines.Expired) as error:
        omissions.append({"kind": "build", "instance_id": None, "side": None,
                          "reason": getattr(error, "reason", "build_failed"),
                          "detail": str(error)[:4096], "reproduction": reproduction(None)})
    parent_probes, parent_afs = {}, {}
    if options.parent_build:
        parent_probes, _, _ = _probes(options.parent_build, parent, budget.deadline, parent_afs)
    host_work = os.path.join(output, WORK_DIR)
    os.makedirs(host_work, 0o700)
    work = host_work
    supervised = None
    if contained is not None:
        work = contained["work"]
        os.mkdir(work, 0o700)
        if supervisor is not None:
            supervised = toolrun.Contained(
                supervisor[0], options.docker, options.image, contained["environment"],
                contained["scratch_root"], options.uid, options.gid, contained["cpus"],
                plan["run_id"])
    elif supervisor is not None:
        supervised = toolrun.Supervisor(supervisor[0])
    execution = {"mode": mode,
                 "containment": "pid_namespace" if contained else "process_group",
                 "declared_containment": declared,
                 "matches_environment": declared == ("linux_container" if contained
                                                     else "trusted_local_process_group"),
                 "supervisor_sha256": supervisor[1] if supervisor is not None else None,
                 "machine_lock": lock_path,
                 "scratch_bound": ("verified_disk_bounded" if contained
                                   else "not_enforced_in_trusted_local_mode"),
                 "container": None if contained is None else {
                     "image": options.image, "runtime": contained["runtime"][:256],
                     "cpus": list(contained["cpus"]),
                     "scratch_limit_bytes": contained["environment"]["scratch_limit_bytes"]}}
    configured = configured_tools(options.perf, options.inferno_bin, options.samply,
                                  options.valgrind_bin)
    checks = os.path.join(work, "capability")
    os.makedirs(checks, 0o700)

    def binds(path):
        # A contained version or capability check sees its executable and its own directory.
        return ((path, False), (checks, True)) if contained is not None else ()

    tools = probe_tools(registry, configured, host, entry["tools"], supervised,
                        budget.deadline, cwd=checks, binds=binds)
    if "perf" in tools and tools["perf"][1] is not None:
        if supervised is None:
            tools["perf"] = (tools["perf"][0], None)
        else:
            available, reason, detail, _ = perfcapture.probe_capability(
                supervised, tools["perf"][1], registry["capture"]["event"], frequency, checks,
                TOOL_ENV, budget.tool_seconds, deadline=budget.deadline,
                binds=binds(tools["perf"][1]))
            tools["perf"][0]["detail"] = ("capability: " + detail)[:4096] if detail else ""
            if not available:
                tools["perf"] = (tools["perf"][0], None)
                omissions.append({"kind": "cpu_profile", "instance_id": None, "side": None,
                                  "reason": reason, "detail": detail[:4096],
                                  "reproduction": reproduction(None)})
    admission.remove_tree(checks)
    cli = None
    if authority_factory is None:
        authority = runner.Authority(git, plan, os.path.join(host_work, "authority"))
        doc, golden = authority.doc, authority.golden
        if authority.cli is not None:
            versions = clicases.side_versions(git, plan)
            adapters = {}
            for side, version in versions.items():
                adapter_id, adapter = clicases.choose_adapter(authority.cli, version)
                if adapter_id is not None:
                    adapters[side] = (adapter_id, adapter)
            cli = CliAuthority(authority.cli, authority.fixtures, versions, adapters)
    else:
        made = authority_factory(plan)
        doc, golden = made[0], made[1]
        cli = made[2] if len(made) > 2 else None
    profile = Profile(output, parent, plan, registry, sha256_bytes(raw), options.recipe,
                      selection, cases, doc, golden, probes, parent_probes, tools, budget,
                      reproduction, builds_sha, host, omissions, supervisor=supervised,
                      execution=execution, cli=cli, afs=afs, parent_afs=parent_afs, work=work,
                      telemetry=options.telemetry)
    result = profile.run()
    finish(output, result, profile.trace_docs, budget)
    actionable = [item for item in result["omissions"] if item["reproduction"] is not None]
    message = "%d captures, %d flamegraphs, %d traces, %d counters, %d telemetry captures, %d " \
        "omissions" % (len(result["captures"]), len(result["renders"]), len(result["traces"]),
                       len(result["counters"]), len(result["telemetry"]["captures"]),
                       len(result["omissions"]))
    if profile.retained:
        # A process the run started may still be live: the operator recovers what was kept.
        paths = sorted(set(profile.retained + list(getattr(profile, "unsafe_staging", ()))))
        message += "; scratch kept for recovery: " + ", ".join(paths)
    return (4 if actionable or profile.retained else 0), os.path.join(output, PROFILES_FILE), \
        message
