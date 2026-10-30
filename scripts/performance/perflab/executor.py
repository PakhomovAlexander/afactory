"""Independent bounded performance execution and original journal (ADR-0240).

Only installed control Python runs on the host. Git revisions are data; builds and samples
use the existing contained laboratory. Transport is injected only in the Actions branch.
"""
import contextlib
import fcntl
import json
import os
import shutil
import signal
import stat
import subprocess
import time
import types

from . import (admission, archive, budget, builds, calibration, clicases, compare, contracts,
               diagnostics,
               gitrefs, history, host, hostlock, hostobserver, hostprep, planning, report, runner,
               telemetrybuild, telemetrylab, tomlfile)
from . import adapters as adapters_module
from .values import ContractError, add_note, canonical_digest, require, sha256_bytes

ROOT = telemetrylab.ROOT
TERMINAL = ("complete", "failed", "interrupted", "uncertain", "submitted", "expired")
RESERVE = 30
EVIDENCE_PREFIXES = ("plan.json", "cohort.json", "parameters.json", "parent-verdict.json",
                     "sources.git", "run", "report", "profile", "baseline-build", "candidate-build")
PARENT_VERDICT = "parent-verdict.json"
PERF_PARANOID = "/proc/sys/kernel/perf_event_paranoid"


def serial_bounds(plan, run_seconds):
    """The local backend's worst-case serial schedule of a core plan, in seconds.

    The local executor builds once and then runs the plan's shards one after another, each
    bounded by the plan's frozen sampling window (start and end health sentinels included),
    under the run deadline the build starts and the executor's own `run_seconds` wall, with the
    runner's finish reserve kept for publication and cleanup. Hosted Actions runs the shards in
    parallel; its feasibility is not this one.
    """
    limits = plan["limits"]
    shards = len(plan["shards"])
    build = limits["build_minutes"] * 60
    sampling = limits["shard_sampling_minutes"] * 60
    reserve = runner.FINISH_RESERVE_SECONDS
    needed = build + shards * sampling + reserve
    available = min(limits["run_total_minutes"] * 60, run_seconds)
    fitting = max(0, (available - build - reserve) // sampling)
    return {"backend": "local_serial", "shards": shards, "build_seconds": build,
            "shard_sampling_seconds": sampling, "finish_reserve_seconds": reserve,
            "needed_seconds": needed, "run_total_seconds": limits["run_total_minutes"] * 60,
            "run_seconds": run_seconds, "available_seconds": available,
            "fitting_shards": fitting, "feasible": needed <= available}


# What a later partition spends before its first shard: verifying the first partition's
# retained build and evidence, retaining its own sources and preparing its scratch.
REUSE_SECONDS = 600


# Sequential bounds of the steps before and after an original's measurement, shared by
# preflight, submission and execution (`time_bounds`): retaining the source authority (and a
# telemetry side's worktree), each contained telemetry build's whole lifetime with its own
# cleanup (`telemetrybuild.run`), and preparing and cleaning one scratch image.
SOURCE_SECONDS = 600
TELEMETRY_BUILD_SECONDS = 4800


def time_bounds(c, work, plan=None, installed=ROOT):
    """(needed seconds, [(step, seconds)]) of one telemetry or profile original's sequential
    worst case: the executor's reserve, its sources, its builds, its scratch and its declared
    measurement (the telemetry profile's `timeout_minutes`, the diagnostic `total_minutes`);
    None for core work, whose bound is its serial schedule (`serial_bounds`)."""
    if work == "core":
        return None
    parts = [("executor reserve", RESERVE), ("source retention", SOURCE_SECONDS)]
    if work == "telemetry":
        t = c["telemetry"]
        cfg = telemetrylab.configuration(telemetrylab.load_registry(installed)[0], t["profile"],
                                         t["experiments"], t["rounds"], t["pairs"], t["scale"],
                                         t["corpus_runs"])
        parts += [("baseline build", TELEMETRY_BUILD_SECONDS),
                  ("candidate build", TELEMETRY_BUILD_SECONDS),
                  ("scratch preparation and cleanup", 2 * hostprep.STEP_SECONDS),
                  ("telemetry profile %s" % t["profile"], cfg["timeout_minutes"] * 60)]
    else:
        parts += [("parent build", plan["limits"]["build_minutes"] * 60),
                  ("scratch preparation and cleanup", 2 * hostprep.STEP_SECONDS),
                  ("diagnostic profile", c["diagnostic"]["total_minutes"] * 60)]
    return sum(seconds for _, seconds in parts), parts


def require_time(c, work, plan=None, installed=ROOT):
    """Refuse a telemetry or profile original whose sequential worst case does not fit its
    `run_seconds`, naming every bound; nothing is relaxed to fit. Returns the admitted bound
    as text, or "" for core work."""
    bounds = time_bounds(c, work, plan, installed)
    if bounds is None:
        return ""
    needed, parts = bounds
    text = "%s needs %d s (%s); run_seconds allows %d s" % (
        work, needed, ", ".join("%s %d s" % part for part in parts), c["run_seconds"])
    require(needed <= c["run_seconds"], text + ": refused before any claim or build, and no "
            "build cap, profile timeout or deadline is relaxed to fit")
    return text


def native_reserve(c, work, plan=None, installed=ROOT):
    """The bytes one original's native work may write beyond what the root holds now: its
    sources, scratch, builds and shard bundles (core and profile) or its builds and telemetry
    work (telemetry), and the bounded archive and history copies it may materialize. One
    calculation for preflight, submission and the claim; original storage is never evicted."""
    if work in ("core", "profile"):
        reserve = (plan["limits"]["worktree_bytes"] + 3 * 1024 ** 3
                   + plan["profile"]["environment"]["scratch_limit_bytes"]
                   + plan["limits"]["build_artifact_bytes"] * 3
                   + plan["limits"]["shard_bundle_bytes"] * len(plan["shards"]) * 4)
    else:
        registry = telemetrylab.load_registry(installed)[0]
        reserve = 24 * 1024 ** 3 + registry["profiles"][c["telemetry"]["profile"]]["work_bytes"]
    return reserve + 4 * c["archive_bytes"] + c["history_bytes"]


def available_seconds(plan, run_seconds):
    return min(plan["limits"]["run_total_minutes"] * 60, run_seconds)


def partition_seconds(plan, partition):
    """The worst-case serial wall of one partition original: its build (the first) or build
    reuse (later ones), each of its shards' whole sampling windows, and the finish reserve."""
    limits = plan["limits"]
    fixed = limits["build_minutes"] * 60 if partition["build_from"] is None else REUSE_SECONDS
    return (fixed + len(partition["shards"]) * limits["shard_sampling_minutes"] * 60
            + runner.FINISH_RESERVE_SECONDS)


def require_serial(plan, run_seconds):
    bounds = serial_bounds(plan, run_seconds)
    require(bounds["feasible"], "infeasible on the serial local backend: %(shards)d shards of "
            "%(shard_sampling_seconds)d s sampling after a %(build_seconds)d s build and a "
            "%(finish_reserve_seconds)d s finish reserve need %(needed_seconds)d s, but the run "
            "deadline allows %(available_seconds)d s; at most %(fitting_shards)d shards fit, and "
            "no sampling window, sentinel or deadline is relaxed to fit" % bounds)
    return bounds


def reachable(path, name):
    """Refuse a configured path the executing account cannot reach through every ancestor."""
    current = "/"
    for part in [part for part in path.split("/") if part]:
        require(os.access(current, os.X_OK), "%s: %s is not searchable by uid %d"
                % (name, current, os.geteuid()))
        current = os.path.join(current, part)
    require(os.path.exists(path), "%s: %s does not exist" % (name, path))
    require(os.access(path, os.R_OK | os.X_OK), "%s: %s is not readable by uid %d"
            % (name, path, os.geteuid()))


def observed(config, registry, recipe):
    """The CPU observer a profile of `recipe` runs with: `host_perf` for a CPU-profile recipe
    when the executor configures the trusted host observer, None (perf contained) otherwise."""
    if registry["recipes"][recipe]["kind"] != "cpu_profile":
        return None
    return config["diagnostic"]["observer"]


def diagnostic_tools(config, registry, recipe, mode, paranoid_path=None):
    """The configured tools a profile's recipe needs, checked before anything is built.

    Returns (records, refusal or None). Tools come only from the executor's explicit `perf`
    and `inferno_bin`; nothing is searched for on PATH. A path is not proof the tool runs:
    the native child run still probes each tool's version and perf's actual capture. What is
    refused here is only what makes that probe certain to fail: a missing or unexecutable
    configured tool, or, in the contained mode (whose child holds no capability), a kernel that
    denies every unprivileged perf event. The trusted host observer's capability is not
    guessed from a sysctl: `observer_capability` records an actual attachment.
    """
    d = config["diagnostic"]
    entry = registry["recipes"][recipe]
    configured = diagnostics.configured_tools(d["perf"], d["inferno_bin"])
    found = diagnostics.probe_tools(registry, configured, "linux", entry["tools"])
    records = [record for record, _ in found.values()]
    missing = ["%s: %s" % (name, record["detail"]) for name, (record, path) in sorted(found.items())
               if path is None]
    if missing:
        return records, "recipe %s needs tools this executor cannot run: %s" % (recipe, "; ".join(missing))
    if ("perf" in entry["tools"] and mode == diagnostics.LINUX_CONTAINER
            and observed(config, registry, recipe) is None):
        paranoid_path = paranoid_path or PERF_PARANOID
        try:
            with open(paranoid_path, "rb") as stream:
                paranoid = int(stream.read(64).strip())
        except (OSError, ValueError) as error:
            return records, "cannot read %s: %s" % (paranoid_path, error)
        if paranoid > 2:
            return records, ("kernel.perf_event_paranoid=%d denies every perf event to the "
                             "contained child, which holds no capability; the executor never "
                             "changes host sysctls or grants capabilities" % paranoid)
    return records, None


def source_release(git, sha, side):
    """The `af` release the retained source `sha` declares, from its Cargo.lock."""
    lock = git.read_file(sha, "Cargo.lock")
    require(lock is not None, "the %s source %s has no Cargo.lock: its release, and so its "
            "workload, is unknown" % (side, sha[:12]))
    return clicases.package_version(lock, "%s:Cargo.lock" % sha[:12])


def telemetry_workload(git, sources, experiments, installed=ROOT):
    """Each side's declared telemetry workload, before any claim or build: the overhead and
    invocation-cost bench scripts of the installed authority measure a release only under the
    fixture contract that names the release its retained source declares (`select_contract`),
    never the current contract by default. The read-latency corpus is one for every release.
    Returns the admission's detail; raises ContractError."""
    if not set(experiments) & {"overhead", "invocation_cost"}:
        return ""
    module = telemetrylab.script_module("scripts/telemetry-overhead.py", installed)
    named = []
    for side, key in (("baseline", "baseline_sha"), ("candidate", "candidate_sha")):
        version = source_release(git, sources[key], side)
        try:
            contract = module.select_contract(version)
        except module.BenchError as error:
            raise ContractError("telemetry refused before any claim or build: the %s source "
                                "%s is af %s: %s" % (side, sources[key][:12], version, error))
        named.append("%s af %s under %s" % (side, version, contract))
    return "telemetry workload: " + ", ".join(named)


def cli_workload(git, plan, control, suite_doc):
    """Each side's declared CLI workload, before any claim or build: a plan (a core original's,
    or the parent plan a profile diagnoses) with process cases
    needs the authority's CLI registry and, for the release each side's retained source
    declares, an adapter of it (`clicases.choose_adapter`), as its shards choose, with a
    runnable command for every selected process case and its parameters: a command the
    adapter does not declare, or declares pending, refuses with its reason. Returns the
    admission's detail; raises ContractError."""
    selected = [case for case in plan["cases"] if case["kind"] == "process"]
    if not selected:
        return ""
    raw = git.read_file(control, clicases.CLI_PATH)
    require(raw is not None, "refused before any claim or build: the authority %s has no %s; "
            "its process cases have no CLI workload" % (control[:12], clicases.CLI_PATH))
    try:
        registry = clicases.check_registry(clicases.load_bytes(raw), suite_doc)
    except ContractError as error:
        raise ContractError("refused before any claim or build: the authority %s's CLI "
                            "registry declares no admissible workload: %s" % (control[:12], error))
    named = []
    for side, version in sorted(clicases.side_versions(git, plan).items()):
        adapter_id, adapter = clicases.choose_adapter(registry, version)
        require(adapter_id is not None, "refused before any claim or build: the %s source is "
                "af %s: %s" % (side, version, adapter))
        for case in selected:
            entry = clicases.command(adapter, case["case_id"], case["parameters"])
            if entry is None or "pending" in entry:
                raise ContractError(
                    "refused before any claim or build: the %s source is af %s, whose CLI "
                    "adapter %s has no runnable command for %s: %s" % (
                        side, version, adapter_id, case["instance_id"],
                        entry["pending"] if entry is not None else "not declared"))
        named.append("%s af %s with %s" % (side, version, adapter_id))
    return "CLI workload: " + ", ".join(named)


def library_workload(git, plan, control, suite_doc):
    """Each side's declared native workload, before any claim or build: every plan runs the
    health sentinel through the probe, and each selected library case (with the seed it copies)
    through its native contract. For the release each side's retained source declares, an
    adapter of the authority's registry (`adapters.toml`) must declare that release
    (`min_version`) and serve every such contract (`adapters.coverage`); a compile at the
    build never stands in for that declaration. Returns the admission's detail; raises
    ContractError."""
    raw = git.read_file(control, adapters_module.ADAPTERS_PATH)
    require(raw is not None, "refused before any claim or build: the authority %s has no %s; "
            "its library workload has no declared coverage"
            % (control[:12], adapters_module.ADAPTERS_PATH))
    try:
        registry = adapters_module.check_registry(adapters_module.load_bytes(
            raw, "%s@%s" % (adapters_module.ADAPTERS_PATH, control[:12])), suite_doc)
    except ContractError as error:
        raise ContractError("refused before any claim or build: the authority %s's adapter "
                            "registry declares no admissible library workload: %s"
                            % (control[:12], error))
    needed = {suite_doc["probe"]["sentinel_contract"]: "the health sentinel"}
    for case in plan["cases"]:
        if case["kind"] != "library":
            continue
        probe = suite_doc["cases"][case["case_id"]]["probe"]
        needed.setdefault(probe["contract_id"], case["instance_id"])
        if "seed" in probe:
            needed.setdefault(probe["seed"]["contract_id"], case["instance_id"] + "'s seed")
    named = []
    for side, version in sorted(clicases.side_versions(git, plan).items()):
        adapter_id, why = adapters_module.coverage(registry, version, needed)
        require(adapter_id is not None, "refused before any claim or build: the %s source is "
                "af %s: %s" % (side, version, why))
        named.append("%s af %s with %s" % (side, version, adapter_id))
    return "native workload: " + ", ".join(named)


def build_recipes(config, work, plan=None, partition=None, installed=ROOT, suite=None,
                  registry=None):
    """The build recipes whose image-local tools one local original runs: the plan's (none for
    a later partition, which reuses the first one's build), a profile's diagnostic recipe's
    base beside it, or a telemetry side's trusted toolchain."""
    if work == "telemetry":
        channel = tomlfile.loads(telemetrybuild.read_source(installed, "rust-toolchain.toml"),
                                 "trusted toolchain")["toolchain"]["channel"]
        return [{"toolchain": channel, "os": "linux", "target": None, "builder": "cargo"}]
    if partition is not None and partition["build_from"] is not None:
        return []
    recipes = [plan["profile"]["recipe"]]
    if work == "profile" and suite is not None and registry is not None:
        base = suite["recipes"][registry["recipes"][config["diagnostic"]["recipe"]]["base_recipe"]]
        if base not in recipes:
            recipes.append(base)
    return recipes


# The sample image's own interpreter, which runs every contained driver (the PTY driver, the
# telemetry bench scripts), and the probe it runs there, in one bounded container of exactly
# the containment every sample gets: it names its account, effective capabilities, no-new-
# privileges flag, network interfaces, cgroup v2 namespace, whether its root is writable, and
# whether the standard modules the drivers import load. Nothing of a candidate is in it.
SAMPLE_PYTHON = "/usr/bin/python3"
SAMPLE_SHELL = "/bin/sh"
SAMPLE_PROBE = r"""import json, os, sys
status = dict(line.split(":", 1) for line in open("/proc/self/status") if ":" in line)
try:
    import fcntl, pty, select, signal, termios
    modules = True
except ImportError:
    modules = False
try:
    open("/.af-perf-root-write", "w").close()
    writable = True
except OSError:
    writable = False
nets = sorted(os.listdir("/sys/class/net")) if os.path.isdir("/sys/class/net") else []
print(json.dumps({"uid": os.getuid(), "gid": os.getgid(),
                  "cap_eff": status.get("CapEff", "").strip(),
                  "no_new_privs": status.get("NoNewPrivs", "").strip(), "networks": nets,
                  "cgroup2": os.path.exists("/sys/fs/cgroup/cgroup.controllers"),
                  "root_writable": writable, "modules": modules,
                  "shell": os.access("/bin/sh", os.X_OK),
                  "python": "%d.%d.%d" % sys.version_info[:3]}))
"""
SAMPLE_PROBE_BYTES = 4096


def sample_admission(image):
    """The sample image's actual runtime, required tools and containment, before any build:
    one probe of `SAMPLE_PROBE` by the image's own `SAMPLE_PYTHON`, in a container started as
    every bounded container is (`builds.BuildImage.run_once`: its own name, this account, no
    network, a read-only root, every capability dropped, no new privileges, its removal
    confirmed). A runtime that cannot run it, a missing interpreter, shell or driver module, or
    any containment the container does not have refuses (`builds.Unavailable`); an unconfirmed
    removal is `build_container_unconfirmed`. Returns the record's detail."""
    result = image.run_once([SAMPLE_PYTHON, "-I", "-B", "-c", SAMPLE_PROBE],
                            {"PATH": builds.SYSTEM_PATH, "LC_ALL": "C",
                             "AF_PERF_SAMPLE_PROBE": "1"})
    where = "the sample image %s" % image.reference
    if result.returncode != 0:
        raise builds.Unavailable("sample_tools_unavailable", "%s cannot run %s: exit %d: %s" % (
            where, SAMPLE_PYTHON, result.returncode,
            result.stderr.decode("utf-8", "replace").strip()[-512:]))
    try:
        require(len(result.stdout) <= SAMPLE_PROBE_BYTES, "over its bound")
        facts = json.loads(result.stdout.decode("utf-8"))
        require(isinstance(facts, dict), "not an object")
        found = dict((key, facts[key]) for key in (
            "uid", "gid", "cap_eff", "no_new_privs", "networks", "cgroup2", "root_writable",
            "modules", "shell", "python"))
    except (ContractError, UnicodeError, ValueError, KeyError) as error:
        raise builds.Unavailable("sample_tools_unavailable", "%s answered no probe record: %s"
                                 % (where, error))
    missing = [name for name, present in ((SAMPLE_SHELL, found["shell"]),
                                          ("the driver modules", found["modules"]))
               if present is not True]
    if missing:
        raise builds.Unavailable("sample_tools_unavailable", "%s lacks %s" % (
            where, " and ".join(missing)))
    problems = []
    if (found["uid"], found["gid"]) != (image.uid, image.gid):
        problems.append("it runs as %r:%r, not %d:%d" % (found["uid"], found["gid"],
                                                         image.uid, image.gid))
    if not isinstance(found["cap_eff"], str) or not found["cap_eff"] or set(
            found["cap_eff"]) != {"0"}:
        problems.append("its effective capabilities are %r" % found["cap_eff"])
    if found["no_new_privs"] != "1":
        problems.append("no_new_privs is %r" % found["no_new_privs"])
    if not isinstance(found["networks"], list) or set(found["networks"]) - {"lo"}:
        problems.append("it has the networks %r" % found["networks"])
    if found["cgroup2"] is not True:
        problems.append("it has no cgroup v2 namespace")
    if found["root_writable"] is not False:
        problems.append("its root filesystem is writable")
    if problems:
        raise builds.Unavailable("sample_containment_unavailable", "%s: a probe container "
                                 "lacks the samples' containment: %s" % (
                                     where, "; ".join(problems)))
    return ("%s Python %s and %s run as %d:%d with no network, no capability, no new "
            "privileges, cgroup v2 and a read-only root" % (
                where, str(found["python"])[:32], SAMPLE_SHELL, image.uid, image.gid))


def image_admission(config, recipes, docker="docker"):
    """The configured build and sample images as this account finds them, before any claim or
    build: each pinned by digest, present locally (nothing pulls), Linux and declaring no
    implicit writable volume; in the build image, each recipe's exact toolchain, its
    target and, for a cargo-zigbuild recipe, the configured image-local cargo-zigbuild and Zig
    at their pinned versions: the probes `builds.Builder.check_host` runs, in containers of the
    build image; and in the sample image its actual runtime, tools and containment
    (`sample_admission`). The caller holds the machine's one exclusion. Returns (records, refusal or
    None, uncertain): a probe container whose removal is unconfirmed is uncertain."""
    records = []
    images = []
    for name, key in (("build_image", "build_image"), ("sample_image", "sample_image")):
        try:
            image = builds.BuildImage(docker, config[key], role=name.replace("_", " "))
            image.refuse_implicit_volumes()
        except ContractError as error:
            # Only `docker image inspect` ran for the images so far and no probe container
            # was started, so the image's identity or volume-policy refusal leaves nothing
            # uncertain.
            detail = "%s: %s" % (getattr(error, "reason", "build_image_refused"), error)
            records.append({"name": "image_admission", "state": "unavailable",
                            "detail": detail[:4096]})
            return records, detail, False
        images.append(image)
        records.append({"name": name, "state": "available", "detail": "%s: %s, %s" % (
            config[key], image.id, image.platform)})
    try:
        build = images[0]
        checked = []
        for recipe in recipes:
            key = tuple(recipe.get(field) for field in (
                "toolchain", "os", "target", "builder", "cargo_zigbuild", "zig"))
            if key in checked:
                continue
            checked.append(key)
            toolchain = builds.ContainerToolchain(build, recipe["toolchain"])
            if toolchain.host_os() != recipe["os"]:
                raise builds.Unavailable("host_mismatch", "the recipe builds for %s, but the "
                                         "toolchain host in %s is %s" % (
                                             recipe["os"], build.reference, toolchain.host))
            if recipe["target"] is not None and not toolchain.supports_target(recipe["target"]):
                raise builds.Unavailable("target_unavailable", "target %s is not installed for "
                                         "%s in %s" % (recipe["target"],
                                                       toolchain.rustc_identity, build.reference))
            detail = "%s; %s" % (toolchain.rustc_identity, toolchain.cargo_identity)
            if recipe["builder"] == "cargo_zigbuild":
                if config["zigbuild_bin"] is None:
                    raise builds.Unavailable("builder_unavailable", "the recipe builds with "
                                             "cargo-zigbuild %s and Zig %s; set zigbuild_bin "
                                             "to their directory in the build image" % (
                                                 recipe["cargo_zigbuild"], recipe["zig"]))
                zigbuild = builds.ContainerZigbuild(build, config["zigbuild_bin"],
                                                    recipe["cargo_zigbuild"], recipe["zig"])
                detail += "; " + zigbuild.identity
            records.append({"name": "image_toolchain", "state": "available",
                            "detail": ("%s in %s" % (detail, build.reference))[:4096]})
        records.append({"name": "sample_runtime", "state": "available",
                        "detail": sample_admission(images[1])[:4096]})
    except builds.Unavailable as error:
        detail = "%s: %s" % (error.reason, error)
        records.append({"name": "image_admission", "state": "unavailable",
                        "detail": detail[:4096]})
        return records, detail, error.reason == "build_container_unconfirmed"
    return records, None, False


def observer_capability(config, registry, recipe, workdir, seconds=60, clock=time.monotonic):
    """The trusted host observer's actual capability, as the executing account under the held
    machine exclusion, before any build: the configured perf attached by PID to an owned gated
    process and its samples decoded (`hostobserver.probe`). Returns (record, refusal or None,
    uncertain). Nothing is checked for a recipe that is not observed on the host."""
    if observed(config, registry, recipe) is None:
        return None, None, False
    d = config["diagnostic"]
    os.mkdir(workdir, 0o700)
    try:
        available, reason, detail = hostobserver.probe(
            hostobserver.bounded_start(diagnostics.TOOL_ENV, workdir, clock), d["perf"],
            registry["capture"]["event"], d["frequency_hz"], workdir, clock() + seconds, clock)
    finally:
        problems = admission.remove_tree(workdir)
    uncertain = reason == "cleanup_uncertain" or bool(problems)
    record = {"name": "host_observer", "state": "available" if available else "unavailable",
              "detail": (detail if available else "%s: %s" % (reason, detail))[:4096]}
    refusal = None if available and not problems else (
        "the trusted host observer cannot record: " + record["detail"]
        + ("; " + "; ".join(problems) if problems else ""))
    return record, refusal, uncertain


class JournalBusy(ContractError):
    """A bounded, pre-submission refusal; no original was launched or consumed."""


def read(path, maximum=8 * 1024 * 1024):
    parent, name = os.path.split(path)
    fd, info = admission.open_confined(parent, name, "executor receipt")
    try:
        require(info.st_size <= maximum, "executor receipt exceeds byte bound")
        chunks = []
        admission.read_bounded(fd, info.st_size, None, name, "reading receipt", chunks.append)
        return b"".join(chunks)
    finally:
        os.close(fd)


def sync_dir(path):
    with budget.defer_alarm():
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        with budget.defer_alarm():
            os.close(fd)


def atomic(path, doc, name):
    data = contracts.write_bytes(doc, name)
    temporary = path + ".pending"
    # Interrupted publication is uncertain; never overwrite a previous pending receipt.
    fd = None
    try:
        with budget.defer_alarm():
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        require(os.write(fd, data) == len(data), "short executor receipt write")
        os.fsync(fd)
        os.replace(temporary, path)
        sync_dir(os.path.dirname(path))
    finally:
        with budget.defer_alarm():
            if fd is not None:
                os.close(fd)


def private_directory(path, create=False):
    require(os.path.abspath(path) == os.path.realpath(path), "executor path has a link or alias")
    if create and not os.path.exists(path):
        os.mkdir(path, 0o700)
        sync_dir(os.path.dirname(path))
    info = os.lstat(path)
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.geteuid()
            and stat.S_IMODE(info.st_mode) & 0o077 == 0,
            "executor state must be a private directory owned by this account")


def load_config(path):
    info = os.lstat(path)
    require(stat.S_ISREG(info.st_mode) and info.st_uid in (0, os.geteuid())
            and not info.st_mode & 0o022, "operator configuration is writable by others or a link")
    return contracts.validate_executor(tomlfile.loads(read(path), path))


def tree_usage(path):
    """Bound controller storage using the laboratory's confined, bounded enumeration."""
    ceiling = contracts.limits.ARCHIVE_BYTES_MAX
    entries = admission.scan(path, admission.Bounds(200000, 64, ceiling, ceiling),
                             label="executor storage")
    used = 0
    root = admission.open_root(path, "executor storage")
    try:
        for entry in entries:
            fd = admission.open_entry(root, entry, "executor storage")
            try:
                info = os.fstat(fd)
                used += max(info.st_size, getattr(info, "st_blocks", 0) * 512)
            finally:
                os.close(fd)
    finally:
        os.close(root)
    return used


def environment(config):
    """Backend/environment identity, independent of a plan's randomized ID and source pair."""
    return canonical_digest({"backend": config["backend"], "machine_id": config["machine_id"],
        "lifetime": "persistent_vm" if config["backend"] == "local" else "fresh_hosted",
        "build_image": config["build_image"], "sample_image": config["sample_image"],
        "zigbuild_bin": config["zigbuild_bin"],
        "core": config["core"], "telemetry": config["telemetry"],
        "diagnostic": config["diagnostic"]})


def source_repository(config, timing=None):
    """Only the exact configured operator mirror gets cross-account Git trust."""
    path = config["repository_path"]
    require(os.path.realpath(path) == path, "operator mirror path must have no links or aliases")
    if timing is None:
        timing = budget.Budget(time.monotonic() + config["cycle_seconds"], time.monotonic, 0)
    return gitrefs.Git(path, budget=timing, trusted_repository=True)


def producer(git, control, installed=ROOT):
    """Require installed trusted measuring code to equal the pinned Git producer.

    No candidate module is imported. Full regular file set, including schemas/telemetry
    scripts, is checked against Git before work is queued; extra executable authority refuses.
    """
    registry = telemetrylab.load_registry(installed)[0]
    members = telemetrylab.read_authority(git, control)
    require(set(telemetrylab.authority_files(registry, installed)) == set(members),
            "installed authority file set differs from the pinned producer")
    actual = telemetrylab.read_files(installed, sorted(members), telemetrylab.AUTHORITY_BYTES_MAX)
    require(members == actual, "installed control bytes differ from the configured control SHA")
    # authority_files additionally refuses links, caches and extra source files.
    digest, _ = telemetrylab.authority_digest(registry, installed)
    return digest


class Journal:
    """One append-only set of original identities, with atomic mutable state receipts.

    A directory is the original reservation. Torn directories, pending writes and claims
    are never silently requeued. Capacity refuses new work rather than deleting originals.
    """
    # The machine lock's directory (tests only); the anchored system lock otherwise.
    lock_directory = None

    @staticmethod
    def free_bytes(path):
        """The bytes the filesystem holding `path` has available to this account."""
        return shutil.disk_usage(path).free

    def __init__(self, config, readonly=False):
        self.config = contracts.validate_executor(config)
        self.root = config["root"]
        self.fd = None
        # A reader (`status`, `verify`, `overview`) takes no lock and writes nothing: it reads
        # the retained, atomically published state while a controller executes an original.
        self.readonly = readonly
        # `images`' admissions this handle made, by configured images and recipes.
        self.admitted_images = {}

    def __enter__(self):
        if self.readonly:
            private_directory(self.root)
            self.requests = os.path.join(self.root, "requests")
            private_directory(self.requests)
            marker = os.path.join(self.root, "journal.json")
            require(not os.path.lexists(marker + ".pending"),
                    "the original journal index is being published; read it again")
            self.index = contracts.read_json(read(marker), "executor-journal")
            require(self.index["journal_id"] == self.config["journal_id"]
                    and self.index["repository"] == self.config["repository"],
                    "journal identity changed")
            return self
        private_directory(self.root, create=True)
        with budget.defer_alarm():
            self.fd = os.open(os.path.join(self.root, "journal.lock"),
                              os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        try:
            info = os.fstat(self.fd)
            require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1, "unsafe journal lock")
            try:
                fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise JournalBusy("another controller holds this original journal; no new submission "
                                  "or launch; retry the unstarted operation later") from error
            self.requests = os.path.join(self.root, "requests")
            private_directory(self.requests, create=True)
            marker = os.path.join(self.root, "journal.json")
            identity = {"schema": "af.perf-executor-journal/1", "journal_id": self.config["journal_id"],
                        "repository": self.config["repository"], "originals": []}
            if not os.path.exists(marker):
                require(not os.listdir(self.requests), "original journal index is missing")
                atomic(marker, identity, "executor-journal")
            require(not os.path.lexists(marker + ".pending"), "torn original journal index")
            self.index = contracts.read_json(read(marker), "executor-journal")
            require(all(self.index[k] == identity[k] for k in ("schema", "journal_id", "repository")),
                    "journal identity changed")
            return self
        except BaseException:
            self.__exit__()
            raise

    def __exit__(self, *exc):
        with budget.defer_alarm():
            if self.fd is not None:
                fd, self.fd = self.fd, None
                os.close(fd)

    def capacity(self, reserve=0, what=None, max_bytes=None):
        """Admit `reserve` more bytes below the root within `max_bytes` (the configuration's)
        and within what the root's filesystem has available to this account now: the policy
        bound and the physical space are both required, and neither stands in for the other.
        A refusal names the bytes held, the reserve, the bound, the policy room and the
        filesystem's available bytes. Returns the admitted figures as text."""
        bound = self.config["max_bytes"] if max_bytes is None else max_bytes
        used = tree_usage(self.root)
        free = self.free_bytes(self.root)
        text = ("storage: %d bytes held + %d-byte %s = %d bytes of max_bytes %d (%d available); "
                "filesystem: %d bytes available for the %d-byte %s" % (
                    used, reserve, what or "reserve", used + reserve, bound,
                    max(0, bound - used), free, reserve, what or "reserve"))
        require(used + reserve <= bound, "executor root capacity exhausted; retain originals "
                "and stop submission: " + text)
        require(reserve <= free, "executor root filesystem has too little free space; retain "
                "originals and stop submission: " + text)
        return text

    def names(self):
        names = sorted(os.listdir(self.requests))
        require(len(names) <= self.config["max_requests"], "original journal exceeds request bound")
        indexed = [item["request_id"] for item in self.index["originals"]]
        require(len(set(indexed)) == len(indexed) and names == sorted(indexed),
                "original journal membership changed or its publication was interrupted")
        for name in names:
            contracts.ID.validate(name, "request directory")
            private_directory(os.path.join(self.requests, name))
        for item in self.index["originals"]:
            require(canonical_digest(self.request(item["request_id"])) == item["request_digest"],
                    "original request changed after reservation")
        return names

    def path(self, ident):
        contracts.ID.validate(ident, "request ID")
        return os.path.join(self.requests, ident)

    def request(self, ident):
        return contracts.read_json(read(os.path.join(self.path(ident), "request.json")),
                                   "executor-request")

    def state(self, ident):
        doc = contracts.read_json(read(os.path.join(self.path(ident), "state.json")), "executor-state")
        require(doc["request_id"] == ident and doc["request_digest"] == canonical_digest(self.request(ident)),
                "state belongs to another original")
        return doc

    def transition(self, request, state, detail="", result=None):
        ident = request["request_id"]
        old = self.state(ident)
        require(old["state"] not in TERMINAL, "terminal originals cannot be retried or replaced")
        require((old["state"], state) in (("queued", "claimed"), ("queued", "expired"),
                ("claimed", "expired"), ("claimed", "complete"),
                ("claimed", "failed"), ("claimed", "interrupted"), ("claimed", "uncertain"),
                ("claimed", "submitted")), "invalid original transition")
        now = int(time.time() * 1000)
        doc = dict(old, state=state, detail=str(detail)[:4096])
        if state == "claimed":
            doc["started_unix_ms"] = now
        else:
            doc.update(ended_unix_ms=now, result_sha256=result)
        atomic(os.path.join(self.path(ident), "state.json"), doc, "executor-state")
        return doc

    def recover(self):
        """A lost claim is uncertain, never a second launch. No fence is cleared here."""
        for ident in self.names():
            path = self.path(ident)
            require(not any(n.endswith(".pending") for n in os.listdir(path)),
                    "torn original publication requires operator inspection: " + ident)
            request, state = self.request(ident), self.state(ident)
            if state["state"] == "claimed":
                self.transition(request, "uncertain", "controller lost after claim; original is never replayed")

    def submit(self, *args, **kwargs):
        timing = budget.Budget(time.monotonic() + self.config["cycle_seconds"], time.monotonic, 0)
        with timing.enforce("freezing original request"):
            return self._submit(*args, **kwargs)

    def exclusive(self, what, check):
        """`check()` -> (result, refusal, uncertain) under the machine's one exclusion, taken
        for it and settled: a busy machine refuses the check; an unconfirmed end keeps the
        fence."""
        try:
            if self.lock_directory is None:
                hostlock.read_anchor(required=True)
            lock = hostlock.MachineLock(self.lock_directory)
        except hostlock.Refused as refused:
            if refused.reason == "concurrent_shard":
                raise JournalBusy("the %s needs the machine exclusion, which another "
                                  "experiment holds: %s" % (what, refused))
            raise ContractError(str(refused))
        try:
            lock.guard()
            result, refusal, uncertain = check()
            if uncertain:
                lock.uncertain = True
            else:
                lock.confirm_clean()
            return result, refusal
        finally:
            lock.release()

    def capability(self, config, registry, recipe):
        """`observer_capability` under the machine's one exclusion."""
        return self.exclusive("host observer capability check", lambda: observer_capability(
            config, registry, recipe, os.path.join(self.root, "observer-probe")))

    def images(self, config, recipes):
        """`image_admission` under the machine's one exclusion, once per configured images and
        recipes for this journal handle (a comparison's members share one check)."""
        key = canonical_digest({"build_image": config["build_image"],
                                "sample_image": config["sample_image"],
                                "zigbuild_bin": config["zigbuild_bin"], "recipes": recipes})
        if key not in self.admitted_images:
            self.admitted_images[key] = self.exclusive(
                "image and tool admission", lambda: image_admission(config, recipes))
        return self.admitted_images[key]

    def preflight(self, work, base, head, key, kind="manual", *, parent=None, **kwargs):
        """Everything a submission checks, as the account that runs it, without reserving an
        original: the `af.perf-executor-preflight/1` document, `ready` or `refused` with the
        exact first refusal, and the local serial bounds and diagnostic tools it checked."""
        timing = budget.Budget(time.monotonic() + self.config["cycle_seconds"], time.monotonic, 0)
        checked = {"serial": None, "tools": []}
        try:
            with timing.enforce("preflight"):
                for key_name in ("root", "repository_path"):
                    reachable(self.config[key_name], key_name)
                request = self._submit(work, base, head, key, kind, parent=parent,
                                       checked=checked, **kwargs)
        except ContractError as error:
            return {"schema": "af.perf-executor-preflight/1", "work": work, "status": "refused",
                    "detail": str(error)[:4096], "request": None, "serial": checked["serial"],
                    "tools": checked["tools"]}
        return {"schema": "af.perf-executor-preflight/1", "work": work, "status": "ready",
                "detail": "", "request": request, "serial": checked["serial"],
                "tools": checked["tools"]}

    def _submit(self, work, base, head, key, kind="manual", *, git=None, installed=ROOT,
               cohort=None, member=None, parameters=None, parent=None, checked=None,
               config=None, frozen_parent=None, frozen_plan=None, partition=None,
               bind_configuration=False):
        """`config` is a comparison lane's derived configuration (its diagnostic recipe);
        `frozen_parent` is (parent record, plan, verdict bytes) of a comparison's verified
        aggregate; `frozen_plan` and `partition` make a core original one partition of a
        comparison's frozen plan. With `bind_configuration` (a comparison's member) an existing
        original of the key is returned only when it froze this very configuration."""
        require(work in ("core", "telemetry", "profile"), "unknown experiment kind")
        require(parent is None or work == "profile", "only a profile names a parent comparison")
        require(frozen_parent is None or (work == "profile" and parent is None),
                "a comparison's aggregate parent is a profile's only parent")
        require((frozen_plan is None) == (partition is None) and (partition is None
                or work == "core"), "only a core partition carries a comparison's frozen plan")
        timing = budget.Budget(time.monotonic() + self.config["cycle_seconds"], time.monotonic, 0)
        git = git or source_repository(self.config, timing)
        c = contracts.validate_executor(config) if config is not None else self.config
        control = git.resolve(c["control_sha"], "installed control")
        require(control == c["control_sha"], "control must be an exact full SHA")
        trusted = git.exact_ref(c["trusted_ref"], "reviewed main")
        require(trusted is not None and git.is_ancestor(control, trusted), "control is not reviewed main authority")
        sources = {"baseline_sha": git.resolve(base, "baseline"), "candidate_sha": git.resolve(head, "candidate"),
                   "control_sha": control, "authority_sha": control}
        submission = {"kind": kind, "key": key}
        ident = "exp_" + canonical_digest({"submission": submission, "work": work})[7:55]
        if ident in self.names() and checked is None:
            original = self.request(ident)
            require(original["sources"] == sources and original["submission"] == submission
                    and original["work"] == work, "original submission key was reused for another source")
            wanted = parent if frozen_parent is None else frozen_parent[0]["request_id"]
            require(original["parent"] is None if wanted is None
                    else (original["parent"] or {}).get("request_id") == wanted,
                    "original submission key was reused for another parent comparison")
            require(original["partition"] == partition,
                    "original submission key was reused for another comparison partition")
            # A comparison member's key under another frozen configuration or plan is another
            # workload (a narrower telemetry profile, partial core cases), never its original.
            # A plain resubmission keeps the original it froze, whatever the journal says now.
            require(not bind_configuration
                    or original["configuration_digest"] == canonical_digest(c),
                    "original submission key was reused for another frozen configuration or "
                    "declared workload")
            require(frozen_plan is None or original["plan_sha256"] == sha256_bytes(
                contracts.write_bytes(frozen_plan, "run")),
                    "original submission key was reused for another frozen plan")
            return original
        require(ident not in self.names(), "the submission key already names an original")
        require(len(self.names()) < c["max_requests"], "original journal is full")
        require(sum(self.state(i)["state"] == "queued" for i in self.names()) < c["max_queue"], "queue is full")
        produced = producer(git, control, installed)
        env = environment(c)
        if cohort is not None:
            contracts.validate_cohort(cohort)
            require(cohort["schema"] == "af.perf-local-cohort/1" and c["backend"] == "local",
                    "local dispatch accepts only a separately declared local cohort")
            require(cohort["dispatch"] == {"journal_id": c["journal_id"], "machine_id": c["machine_id"],
                    "environment_digest": env, "producer_digest": produced}, "local cohort dispatch authority mismatch")
            require(kind == "calibration" and work == "core" and member is not None,
                    "a calibration original is a declared core member")
            declaration = git.read_file(trusted, "fixtures/performance/local-cohort.json")
            require(declaration is not None and contracts.read_json(declaration, "local-cohort-declaration") == cohort,
                    "local declaration must be committed and reviewed on main before dispatch")
            declared = next((m for m in cohort["members"] if m["member_id"] == member), None)
            require(declared is not None, "unknown local member")
            require(key == canonical_digest(cohort) + ":" + member, "local original key must bind cohort/member")
            start = calibration.start_unix_ms(cohort) + declared["day"] * calibration.DAY_MS
            require(start <= int(time.time() * 1000) < start + calibration.DAY_MS, "member is off schedule")
            if declared["kind"] in ("injection", "power"):
                require(parameters is not None, "trial needs committed, reviewed sealed parameters")
                # Review/commit checkpoint: parameters are data at the exact trusted ref.
                sealed_raw = git.read_file(trusted, "fixtures/performance/local-calibration-parameters.json")
                require(sealed_raw is not None and contracts.read_json(sealed_raw, "calibration-parameters") == parameters,
                        "sealed parameters must be committed on reviewed main at the documented path")
        elif kind == "calibration":
            raise ContractError("calibration submissions need their immutable declaration")
        plan = parent_record = verdict_bytes = suite = registry = None
        if partition is not None:
            plan = frozen_plan
        if work == "profile" and c["backend"] == "local":
            d = c["diagnostic"]
            suite = planning.read_suite(git, control)
            raw = git.read_file(control, diagnostics.REGISTRY_PATH)
            require(raw is not None, "control has no diagnostic registry")
            registry = diagnostics.load_bytes(raw, diagnostics.REGISTRY_PATH, suite.doc)
            require(d["recipe"] in registry["recipes"], "diagnostic.recipe is not in the registry")
            records, refusal = diagnostic_tools(c, registry, d["recipe"], diagnostics.LINUX_CONTAINER)
            if checked is not None:
                checked["tools"] = [{"name": r["name"], "state": r["state"], "detail": r["detail"]}
                                    for r in records]
            require(refusal is None, "profile refused before any build: " + str(refusal))
            if checked is not None and observed(c, registry, d["recipe"]) is not None:
                # The actual attachment, not a path or sysctl guess, before anything is built.
                record, refusal = self.capability(c, registry, d["recipe"])
                checked["tools"].append(record)
                require(refusal is None, "profile refused before any build: " + str(refusal))
            if frozen_parent is not None:
                require(not d["cases"], "a parent-selected profile names no diagnostic.cases")
                parent_record, plan, verdict_bytes = frozen_parent
            elif parent is not None:
                require(not d["cases"], "a parent-selected profile names no diagnostic.cases")
                # Only a verified completed local core original selects; its retained plan
                # and verdict are this profile's exact parent identity.
                parent_request, parent_state, _ = verify_original(self, parent, installed)
                require(parent_request["work"] == "core" and parent_request["sources"] == sources,
                        "the parent is not a core comparison of these exact sources")
                parent_path = self.path(parent)
                plan = contracts.read_json(read(os.path.join(parent_path, "plan.json")), "run")
                verdict_bytes = read(os.path.join(parent_path, "report", report.VERDICT_JSON))
                verdict = contracts.read_json(verdict_bytes, "verdict")
                require(verdict["run_id"] == plan["run_id"]
                        and verdict["plan_digest"] == plan["plan_digest"],
                        "the parent verdict is of another plan")
                parent_record = {"request_id": parent,
                    "request_digest": canonical_digest(parent_request),
                    "result_sha256": parent_state["result_sha256"],
                    "plan_sha256": parent_request["plan_sha256"],
                    "verdict_sha256": sha256_bytes(verdict_bytes)}
            else:
                require(d["cases"], "a profile without diagnostic.cases selects from a verified "
                        "parent comparison: submit with --parent REQUEST_ID")
        if plan is None and work in ("core", "profile") and c["backend"] == "local":
            core = c["core"]
            plan = planning.plan(git, planning.Request(trigger="manual", tier=core["tier"],
                scope="calibration" if cohort else ("partial_scope" if core["cases"] else "bootstrap"),
                base=sources["baseline_sha"], head=sources["candidate_sha"], control=control,
                authority=control, cases=core["cases"], environment=core["environment"],
                build_image=c["build_image"], shards=core["shards"], run_id="local_" + ident[4:],
                cohort=cohort, member=member, calibration=parameters))
            require(not plan["required_check_eligible"], "local/advisory request cannot satisfy required checks")
            if c["backend"] == "local":
                require(plan["profile"]["environment"]["runner"] == "dedicated"
                        and plan["profile"]["environment"]["containment"] == "linux_container",
                        "local core needs a dedicated contained environment")
            if work == "core":
                if checked is not None:
                    checked["serial"] = serial_bounds(plan, c["run_seconds"])
                require_serial(plan, c["run_seconds"])
        elif work == "core" and partition is not None:
            require(c["backend"] == "local" and cohort is None,
                    "a comparison partition is a local, non-calibration core original")
            require(plan["run_id"] == "local_" + partition["comparison_id"][4:]
                    and plan["resolved"] == sources
                    and set(partition["shards"]) <= set(s["index"] for s in plan["shards"]),
                    "a partition executes shards of its comparison's frozen plan")
            require(partition_seconds(plan, partition) <= available_seconds(plan, c["run_seconds"]),
                    "the partition does not fit one bounded local original")
        elif work == "telemetry":
            registry = telemetrylab.load_registry(installed)[0]
            t = c["telemetry"]
            chosen = telemetrylab.configuration(registry, t["profile"], t["experiments"],
                                                t["rounds"], t["pairs"], t["scale"],
                                                t["corpus_runs"])
        if c["backend"] == "local":
            # The selected releases' declared workloads, from their retained sources under
            # this authority, before anything is claimed or built: a release no contract or
            # adapter names, or a selected case its adapter leaves pending, is refused, never
            # measured under the current fixture or left to a compile.
            if work == "telemetry":
                workloads = [telemetry_workload(git, sources, chosen["experiments"], installed)]
            else:
                suite_doc = (suite or planning.read_suite(git, control)).doc
                workloads = [cli_workload(git, plan, control, suite_doc),
                             library_workload(git, plan, control, suite_doc)]
            # The same time and storage admission execution applies, before anything is
            # reserved, claimed or built (`time_bounds`, `native_reserve`).
            admitted = workloads + [require_time(c, work, plan, installed),
                        self.capacity(native_reserve(c, work, plan, installed),
                                      "native reserve", c["max_bytes"])]
            if checked is not None:
                checked["admission"] = "; ".join(text for text in admitted if text)
            # The configured images and their image-local tools, as execution checks them
            # again before its first build (`image_admission`).
            records, refusal = self.images(c, build_recipes(
                c, work, plan, partition, installed, suite and suite.doc, registry))
            if checked is not None:
                checked["tools"] = (checked["tools"] + records)[:16]
            require(refusal is None, "%s refused before any claim or build: the configured "
                    "images cannot run it: %s" % (work, refusal))
        request = {"schema": "af.perf-executor-request/2", "request_id": ident,
            "backend": c["backend"], "journal_id": c["journal_id"], "machine_id": c["machine_id"],
            "configuration_digest": canonical_digest(c), "sources": sources, "submission": submission,
            "work": work, "created_unix_ms": int(time.time() * 1000), "environment_digest": env,
            "producer_digest": produced, "plan_sha256": None if plan is None else sha256_bytes(contracts.write_bytes(plan, "run")),
            "cohort_digest": None if cohort is None else canonical_digest(cohort), "member_id": member,
            "parameters_sha256": None if parameters is None else sha256_bytes(contracts.write_bytes(parameters, "calibration-parameters")),
            "parent": parent_record, "partition": partition}
        contracts.CONTRACTS["executor-request"][2](request, "request")
        self.capacity(16 * 1024 * 1024)
        if checked is not None:
            return request
        path = self.path(ident)
        os.mkdir(path, 0o700)  # original identity precedes every possible launch
        sync_dir(self.requests)
        atomic(os.path.join(path, "request.json"), request, "executor-request")
        for filename, doc, contract in (("configuration.json", c, "executor"), ("plan.json", plan, "run"),
                ("cohort.json", cohort, "local-cohort-declaration"), ("parameters.json", parameters, "calibration-parameters")):
            if doc is not None:
                atomic(os.path.join(path, filename), doc, contract)
        if verdict_bytes is not None:
            with open(os.path.join(path, PARENT_VERDICT), "xb") as stream:
                stream.write(verdict_bytes)
                stream.flush()
                os.fsync(stream.fileno())
        self.index["originals"].append({"request_id": ident, "request_digest": canonical_digest(request)})
        atomic(os.path.join(self.root, "journal.json"), self.index, "executor-journal")
        atomic(os.path.join(path, "state.json"), {"schema": "af.perf-executor-state/2",
            "request_id": ident, "request_digest": canonical_digest(request), "state": "queued",
            "started_unix_ms": None, "ended_unix_ms": None, "result_sha256": None, "detail": ""}, "executor-state")
        return request

    def poll(self, work="core", **kwargs):
        git = source_repository(self.config, budget.Budget(
            time.monotonic() + self.config["cycle_seconds"], time.monotonic, 0))
        tip = git.exact_ref(self.config["trusted_ref"], "reviewed main")
        require(tip is not None, "reviewed main is unavailable")
        base = gitrefs.first_parent(git, tip, "reviewed main")
        # Every newly observed reviewed tip is measured once. Skipped intermediate revisions
        # are not claimed as CI coverage; PR/release submissions are explicit operator events.
        return self.submit(work, base, tip, "main:" + tip, "main", git=git, **kwargs)

    def cycle(self, *, execute=None, transport=None, lock_directory=None, installed=ROOT):
        self.recover()
        queued = [i for i in self.names() if self.state(i)["state"] == "queued"]
        if not queued:
            return "idle"
        ident = min(queued, key=lambda i: (self.request(i)["created_unix_ms"], i))
        request = self.request(ident)
        frozen_config = contracts.read_json(read(os.path.join(self.path(ident), "configuration.json")), "executor")
        require(canonical_digest(frozen_config) == request["configuration_digest"], "frozen configuration changed")
        # Machine busy is deliberately before the claim: retrying this queue item has spent
        # no calibration original. There is no retry after a durable claim, even on refusal.
        lock = None
        if request["backend"] == "local":
            try:
                if lock_directory is None:
                    hostlock.read_anchor(required=True)
                lock = hostlock.MachineLock(lock_directory)
            except hostlock.Refused as refused:
                if refused.reason == "concurrent_shard":
                    return "busy"
                raise
        try:
            # Reserve the native bounds before claiming/launching; original storage is
            # never evicted to make progress. Scratch images themselves enforce writes.
            if request["backend"] == "local":
                p = None
                if request["work"] in ("core", "profile"):
                    p = contracts.read_json(read(os.path.join(self.path(ident), "plan.json")), "run")
                # The same reserve the submission admitted (`native_reserve`), under the frozen
                # configuration's own bound, before any original is spent.
                self.capacity(native_reserve(frozen_config, request["work"], p, installed),
                              "native reserve", frozen_config["max_bytes"])
            if calibration_expired(request, self.path(ident)):
                self.transition(request, "expired", "not executed: frozen calibration member day expired")
                return "expired"
            state = self.transition(request, "claimed")
            if lock is not None:
                lock.guard()
            started = state["started_unix_ms"]
            terminal = budget.Budget(time.monotonic() + frozen_config["run_seconds"], time.monotonic, 0)
            cancelled = None
            try:
                operation = execute or execute_local
                if request["backend"] == "local":
                    require(transport is None, "local execution refuses an Actions transport")
                    try:
                        result = operation(frozen_config, request, self.path(ident), lock, installed)
                    except runner.Cancelled as error:
                        # A cancellation whose owned cleanup was confirmed ends this original
                        # interrupted, then goes on: nothing later of this controller runs.
                        if error.result is None:
                            raise
                        result, cancelled = error.result, error
                else:
                    result = dispatch_actions(frozen_config, request, self.path(ident), transport)
                with terminal.enforce("publishing executor terminal evidence"):
                    result.update(schema="af.perf-executor-result/2", request_id=ident,
                        request_digest=canonical_digest(request), backend=request["backend"],
                        machine_id=request["machine_id"], environment_digest=request["environment_digest"],
                        producer_digest=request["producer_digest"], sources=request["sources"],
                        host=host.identity(), required_check=False, started_unix_ms=started,
                        ended_unix_ms=max(started, int(time.time() * 1000)))
                    require(lock is None or not lock.uncertain or result["status"] == "uncertain",
                            "uncertain owned descendants prevent a complete outcome")
                    atomic(os.path.join(self.path(ident), "result.json"), result, "executor-result")
                    digest = sha256_bytes(contracts.write_bytes(result, "executor-result"))
                    self.transition(request, result["status"], result["detail"], digest)
                    # Terminal evidence precedes release; uncertainty never settles a fence.
                    if lock is not None and result["status"] != "uncertain":
                        lock.confirm_clean()
                if cancelled is None:
                    return result["status"]
            except BaseException as error:
                # Claims never replay, including an ambiguous remote handoff or publication.
                try:
                    failure = budget.Budget(min(terminal.deadline, time.monotonic() + 5), time.monotonic, 0)
                    with failure.enforce("recording executor uncertainty"):
                        old = self.state(ident)
                        if old["state"] == "claimed":
                            self.transition(request, "uncertain", "%s: %s" % (type(error).__name__, error))
                        elif old["state"] in TERMINAL:
                            # A failed final fence settlement may only downgrade a terminal
                            # record, never make the original executable again.
                            old.update(state="uncertain", detail=str(error)[:4096])
                            atomic(os.path.join(self.path(ident), "state.json"), old, "executor-state")
                except BaseException as recording:
                    add_note(error, "uncertainty publication unconfirmed; retain original claim: %s" % recording)
                raise
            # Its terminal record is published and its fence settled; the cancellation goes on.
            raise cancelled
        finally:
            if lock is not None:
                lock.release()


def calibration_expired(request, path):
    """Check the original frozen member, immediately at each launch decision.

    A future member is an authority error, never an expired/backfilled replacement.
    """
    if request["cohort_digest"] is None:
        return False
    cohort = contracts.read_json(read(os.path.join(path, "cohort.json")), "local-cohort-declaration")
    require(canonical_digest(cohort) == request["cohort_digest"], "original cohort changed")
    member = next((m for m in cohort["members"] if m["member_id"] == request["member_id"]), None)
    require(member is not None and request["backend"] == "local"
            and request["submission"] == {"kind": "calibration",
                "key": request["cohort_digest"] + ":" + member["member_id"]},
            "unbound calibration original")
    start = calibration.start_unix_ms(cohort) + member["day"] * calibration.DAY_MS
    now = int(time.time() * 1000)
    require(now >= start, "calibration original is before its frozen member day")
    return now >= start + calibration.DAY_MS


def run_git(repo, args, timing, cwd=None):
    env = builds.git_environment(os.devnull)
    return subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "-C", repo, *args],
        cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        timeout=timing.seconds("retaining source Git objects", 120), check=True).stdout


def retain_sources(config, request, path, timing):
    """Shallow exact original commits/trees in a private, credential-free repository.

    Refs pin all four identities independently of future mirror GC. Candidate files, hooks
    and configuration are never checked out or invoked on the controller.
    """
    target = os.path.join(path, "sources.git")
    source_git = source_repository(config, timing)
    objects = set()
    for sha in sorted(set(request["sources"].values())):
        entries = source_git.tree_entries(sha, [])
        require(len(entries) <= 32768 and all(kind == "blob" and mode in ("100644", "100755")
                for mode, kind, _, _ in entries), "source tree exceeds bounds or holds links/submodules")
        objects.update(obj for _, _, obj, _ in entries)
    sizes = source_git._run(["cat-file", "--batch-check"],
                            stdin=("\n".join(sorted(objects)) + "\n").encode()).stdout
    total = 0
    for line in sizes.decode("ascii").splitlines():
        fields = line.split()
        require(len(fields) == 3 and fields[1] == "blob" and 0 <= int(fields[2]) <= 16 * 1024 ** 2,
                "source object exceeds its admission bound")
        total += int(fields[2])
    require(total <= 1024 ** 3, "retained source objects exceed 1 GiB")
    os.mkdir(target, 0o700)
    run_git(target, ["init", "--bare", "--quiet"], timing)
    for sha in sorted(set(request["sources"].values())):
        run_git(target, ["-c", "safe.directory=" + config["repository_path"],
            "fetch", "--quiet", "--depth=1", "--no-tags", "--no-write-fetch-head",
            config["repository_path"], sha + ":refs/heads/source_" + sha], timing)
    require(tree_usage(target) <= 3 * 1024 ** 3, "retained source repository exceeds its reserved bound")
    return target


def file_manifest(root, prefixes, timing=None):
    entries, count = [], 0
    for prefix in prefixes:
        path = os.path.join(root, prefix)
        if not os.path.lexists(path):
            continue
        if os.path.isdir(path):
            ceiling = contracts.limits.ARCHIVE_BYTES_MAX
            found = admission.scan(path, admission.Bounds(8192 - count, 64, ceiling, ceiling),
                                   budget=timing, label="local original evidence")
            hashes = admission.digests(path, found, budget=timing, label="local original evidence")
            count += len(found)
            entries.extend({"path": prefix + "/" + entry.path, "size": entry.size,
                            "sha256": hashes[entry.path]} for entry in found if entry.kind == "file")
        else:
            require(count < 8192, "local evidence exceeds entry bound")
            fd, info = admission.open_confined(root, prefix, "local original evidence")
            try:
                digest = admission.read_bounded(fd, info.st_size, timing, prefix, "hashing local evidence")
            finally:
                os.close(fd)
            entries.append({"path": prefix, "size": info.st_size, "sha256": digest})
            count += 1
    return sorted(entries, key=lambda item: item["path"])


def admit_images(config, work, path, lease, terminal, recipes):
    """The execution's own `image_admission`, under its held exclusion and before its first
    build: an image or image-local tool missing since submission ends the original `failed`,
    nothing built or measured, and only an unconfirmed probe container keeps the fence."""
    _, refusal, uncertain = image_admission(config, recipes)
    if uncertain:
        lease.uncertain = True
        raise ContractError("image admission probe cleanup is uncertain: " + str(refusal))
    if refusal is None:
        return None
    return {"status": "failed", "measurement_status": "unavailable",
            "evidence": file_manifest(path, EVIDENCE_PREFIXES, terminal), "actions": None,
            "detail": ("%s refused before any build, nothing measured: the configured images "
                       "cannot run it: %s" % (work, refusal))[:4096]}


def execute_local(config, request, path, lease, installed=ROOT):
    lease.check()
    start = time.monotonic()
    deadline = start + config["run_seconds"] - RESERVE
    timing = budget.Budget(deadline, time.monotonic, RESERVE)
    terminal = budget.Budget(deadline, time.monotonic, 0)
    git = source_repository(config, timing)
    require(producer(git, request["sources"]["control_sha"], installed) == request["producer_digest"],
            "installed producer changed since submission")
    require(environment(config) == request["environment_digest"], "frozen environment changed")
    with timing.enforce("retaining source authority"):
        sources = retain_sources(config, request, path, timing)
    # A source repository's target/ is the builder's own target, never a shared /tmp cache.
    owner = sources
    run = os.path.join(path, "run")
    scratch = os.path.join(path, "scratch")
    scratch_image = scratch + ".img"
    preparation_host = hostprep.Host(run=lambda argv: hostprep._run(argv, timeout=terminal.seconds(
        "scratch preparation and cleanup", hostprep.STEP_SECONDS)))
    deadline_ms = int(time.time() * 1000 + timing.left() * 1000)
    work = request["work"]
    if work in ("core", "profile"):
        plan = contracts.read_json(read(os.path.join(path, "plan.json")), "run")
        require(sha256_bytes(contracts.write_bytes(plan, "run")) == request["plan_sha256"], "local plan changed")
        # Original plan is frozen before launch; replay is from retained objects, never Actions.
        require(plan["resolved"] == request["sources"], "plan names another source")
        # Source/producer preparation can cross midnight after the durable claim. Check
        # again here, before the first builder (including its container probes) can launch.
        if calibration_expired(request, path):
            return {"status": "expired", "measurement_status": "not_executed_off_schedule",
                    "evidence": file_manifest(path, EVIDENCE_PREFIXES, terminal), "actions": None,
                    "detail": "not executed: frozen calibration member day expired during preparation"}
        suite_doc = registry = None
        if work == "profile":
            # Tools are checked again as the executing compute user, before the first build:
            # an unavailable capability ends here with its exact reason, nothing built.
            git = gitrefs.Git(sources, budget=timing)
            authority = request["sources"]["authority_sha"]
            suite_doc = planning.read_suite(git, authority).doc
            registry = diagnostics.load_bytes(git.read_file(authority, diagnostics.REGISTRY_PATH),
                diagnostics.REGISTRY_PATH, suite_doc)
            recipe = config["diagnostic"]["recipe"]
            _, refusal = diagnostic_tools(config, registry, recipe, diagnostics.LINUX_CONTAINER)
            if refusal is None:
                # The trusted host observer's actual attachment, under this held exclusion.
                _, refusal, uncertain = observer_capability(
                    config, registry, recipe, os.path.join(path, "observer-probe"),
                    clock=time.monotonic)
                if uncertain:
                    lease.uncertain = True
                    raise ContractError("host observer capability cleanup is uncertain: "
                                        + str(refusal))
            if refusal is not None:
                return {"status": "failed", "measurement_status": "unavailable",
                        "evidence": file_manifest(path, EVIDENCE_PREFIXES, terminal),
                        "actions": None, "detail": ("profile refused before any build: "
                                                    + refusal)[:4096]}
        partition = request["partition"]
        refused = admit_images(config, work, path, lease, terminal, build_recipes(
            config, work, plan, partition, installed, suite_doc, registry))
        if refused is not None:
            return refused
        if partition is not None and partition["build_from"] is not None:
            # A later partition runs on the first partition's verified, immutable build: the
            # one build every shard bundle of the comparison binds. Nothing is rebuilt.
            try:
                build_dir, builds_bytes = partition_build(config, request, installed)
            except (ContractError, OSError) as error:
                return {"status": "failed", "measurement_status": "unavailable",
                        "evidence": file_manifest(path, EVIDENCE_PREFIXES, terminal),
                        "actions": None, "detail": ("partition refused before any launch: the "
                                                    "first partition's build is not admitted: %s"
                                                    % error)[:4096]}
            build = types.SimpleNamespace(layout=types.SimpleNamespace(build=build_dir))
            built = contracts.read_json(builds_bytes, "builds")
        else:
            build = builds.Builder(owner, plan, None, bounded_container=True,
                                   zigbuild_bin=config["zigbuild_bin"], deadline_unix_ms=deadline_ms)
            built = build.build()
            require(built["cleanup"]["status"] == "removed", "core build cleanup unconfirmed")
        with timing.enforce("preparing local sample scratch"):
            hostprep.prepare(scratch_image, scratch, plan["profile"]["environment"]["scratch_limit_bytes"],
                             host=preparation_host)
        if work == "core":
            authority = None
            if partition is not None and partition["build_from"] is not None:
                authority = partition_authority(plan, partition, request["request_id"],
                                                builds_bytes, built, int(time.time() * 1000))
            runner.prepare_run_dir(run, contracts.write_bytes(plan, "run"),
                                   builds_bytes if authority is not None
                                   else contracts.write_bytes(built, "builds"), authority)
            for shard in plan["shards"]:
                if partition is not None and shard["index"] not in partition["shards"]:
                    continue
                try:
                    doc = runner.run_shard(runner.Options(
                        plan=os.path.join(path, "plan.json"), build=build.layout.build,
                        shard=shard["index"], run_dir=run, repo=sources,
                        mode=runner.LINUX_CONTAINER, image=config["sample_image"],
                        scratch_fs=scratch, deadline_unix_ms=deadline_ms), machine_lease=lease)
                except runner.Unavailable:
                    # No candidate launched; absent shard remains incomplete, never a pass.
                    require(not lease.uncertain, "preflight scratch cleanup is uncertain")
                    continue
                except runner.Cancelled as cancelled:
                    raise settle_cancellation(cancelled, plan, partition, path, lease, terminal,
                                              lambda: hostprep.cleanup(scratch_image, scratch,
                                                                       host=preparation_host))
                require(not lease.uncertain, "owned shard cleanup is uncertain")
                if doc["failure"] and doc["failure"]["reason"] in (
                        "cleanup_uncertain", "seed_cleanup_failed"):
                    raise ContractError("shard cleanup is uncertain; retained original fence")
            with terminal.enforce("rendering and archiving local measurement"):
                evaluation = compare.evaluate(run)
                report.write(run, os.path.join(path, "report"))
                if partition is not None:
                    # A partition's shards are judged only in its comparison's aggregate,
                    # which the comparison archives once; alone it is never a verdict.
                    measurement = "partition_measured"
                else:
                    archive_root = os.path.join(config["root"], "core-archive")
                    if not os.path.exists(archive_root):
                        archive.Archive.create(archive_root, config["archive_bytes"])
                    with archive.Archive(archive_root) as store:
                        store.import_run(run)
                        if request["cohort_digest"]:
                            store.pin(plan["run_id"], "active_cohort", "Local calibration original")
                        store.retain()
                    history_output = os.path.join(path, "history")
                    history.write([archive_root], history_output)
                    measurement = evaluation.verdict["aggregate"]["result"]
        else:
            d = config["diagnostic"]
            require(timing.left() >= d["total_minutes"] * 60,
                    "remaining local deadline cannot cover the declared diagnostic profile")
            def diagnostic_builder(child, until):
                builder = builds.Builder(owner, child, None, bounded_container=True,
                                         zigbuild_bin=config["zigbuild_bin"], deadline_unix_ms=min(until, deadline_ms))
                doc = builder.build()
                require(doc["cleanup"]["status"] == "removed", "diagnostic build cleanup unconfirmed")
                return builder.layout.build
            # A parent-selected profile reads the verified parent verdict it froze at
            # submission; named cases are an explicit selection that reads none.
            verdict = None
            if request["parent"] is not None:
                verdict = os.path.join(path, PARENT_VERDICT)
                require(sha256_bytes(read(verdict)) == request["parent"]["verdict_sha256"],
                        "the frozen parent verdict changed")
            code, profiles, detail = diagnostics.run_profile(gitrefs.Git(sources, budget=timing),
                diagnostics.Options(plan=os.path.join(path, "plan.json"), cases=d["cases"],
                    verdict=verdict, max_cases=d["max_cases"], perf=d["perf"],
                    inferno_bin=d["inferno_bin"],
                    recipe=d["recipe"], observer=observed(config, registry, d["recipe"]),
                    output=os.path.join(path, "profile"), parent_build=build.layout.build,
                    repo=sources, mode=diagnostics.LINUX_CONTAINER, image=config["sample_image"],
                    scratch_fs=scratch, builder=diagnostic_builder, build_image=config["build_image"],
                    duration_seconds=d["duration_seconds"], frequency_hz=d["frequency_hz"],
                    total_minutes=d["total_minutes"], telemetry=d["telemetry"]), machine_lease=lease)
            doc = contracts.read_json(read(profiles), "profiles") if profiles else None
            require(doc is None or doc["execution"]["retained_scratch"] == 0, "diagnostic cleanup uncertain")
            if profiles is not None:
                with timing.enforce("admitting native diagnostic evidence"):
                    report.admit_children([os.path.join(path, "profile")], plan)
            measurement = "available" if code == 0 else "unavailable"
        require(not lease.uncertain, "owned shard or diagnostic cleanup is uncertain")
        with terminal.enforce("cleaning local scratch"):
            problems = hostprep.cleanup(scratch_image, scratch, host=preparation_host)
            require(not problems, "; ".join(problems))
    else:
        # Before either build: the sequential bounds left must still cover both builds, the
        # scratch and the profile; a known refusal is a clean terminal outcome, nothing built
        # and nothing measured.
        _, parts = time_bounds(config, "telemetry", None, installed)
        rest = [part for part in parts if part[0] not in ("executor reserve", "source retention")]
        left = timing.left()
        if left < sum(seconds for _, seconds in rest):
            return {"status": "failed", "measurement_status": "not_executed",
                    "evidence": file_manifest(path, EVIDENCE_PREFIXES, terminal),
                    "actions": None, "detail": (
                        "telemetry refused before any build, nothing measured: %s need %d s, "
                        "%d s are left of run_seconds %d" % (
                            ", ".join("%s %d s" % part for part in rest),
                            sum(seconds for _, seconds in rest), int(left),
                            config["run_seconds"]))[:4096]}
        refused = admit_images(config, work, path, lease, terminal,
                               build_recipes(config, work, installed=installed))
        if refused is not None:
            return refused
        binaries, records = {}, {}
        for side in contracts.SIDES:
            sha = request["sources"][side + "_sha"]
            source = os.path.join(path, side + "-source")
            run_git(sources, ["worktree", "add", "--detach", "--quiet", source, sha], timing)
            output = os.path.join(path, side + "-build")
            telemetrybuild.run(source, output, config["build_image"], os.path.join(path, side + "-scratch"),
                               sha, seconds=min(TELEMETRY_BUILD_SECONDS,
                                                int(timing.seconds("contained telemetry build"))))
            binaries[side] = os.path.join(output, "af")
            record = {"schema": contracts.TELEMETRY_BUILD_SCHEMA, "side": side, "source_sha": sha,
                "binary_sha256": telemetrylab._binary(binaries[side], side)[1], "image": config["build_image"],
                "rustc": read(os.path.join(output, "af-rustc")).decode().strip()[:512],
                "cargo": read(os.path.join(output, "af-cargo")).decode().strip()[:512],
                "command": "cargo build --release --locked --offline --bin af --target-dir /state/target"}
            records[side] = contracts.validate_telemetry_build(record)
            atomic(os.path.join(output, "build.json"), record, "telemetry-build")
            run_git(sources, ["worktree", "remove", "--force", source], terminal)
        t = config["telemetry"]
        registry = telemetrylab.load_registry(installed)[0]
        cfg = telemetrylab.configuration(registry, t["profile"], t["experiments"], t["rounds"],
                                         t["pairs"], t["scale"], t["corpus_runs"])
        require(timing.left() >= cfg["timeout_minutes"] * 60, "remaining local deadline cannot cover telemetry profile")
        hostprep.prepare(scratch_image, scratch, cfg["work_bytes"], host=preparation_host)
        owner_path = os.path.join(path, "telemetry-owner.json")
        with hostprep.TelemetryOwner(owner_path, scratch, create=True, image=scratch_image) as owner_fence:
            result = telemetrylab.run(telemetrylab.Settings(profile=t["profile"], trigger="manual",
                baseline_af=binaries["baseline"], candidate_af=binaries["candidate"],
                baseline_sha=request["sources"]["baseline_sha"], candidate_sha=request["sources"]["candidate_sha"],
                authority_sha=request["sources"]["authority_sha"], builds=records, output=run,
                result_id="local_" + request["request_id"][4:], mode="linux_container", root=installed,
                image=config["sample_image"], scratch_fs=scratch, scratch_owner=owner_fence,
                experiments=t["experiments"], rounds=t["rounds"], pairs=t["pairs"], scale=t["scale"],
                corpus_runs=t["corpus_runs"]), machine_lease=lease)
            telemetrylab.verify(run, installed)
            require(not owner_fence.cleanup(scratch_image, host=preparation_host), "telemetry scratch cleanup unconfirmed")
        with terminal.enforce("retaining local telemetry history"):
            doc = telemetrylab.history(os.path.join(config["root"], "local-telemetry-history"), [run],
                                       installed, bytes_max=config["history_bytes"])
            require(not doc["refused"], "local telemetry history refused original")
        measurement = result["status"]
    with terminal.enforce("sealing original local evidence"):
        evidence = file_manifest(path, EVIDENCE_PREFIXES, terminal)
    return {"status": "complete", "measurement_status": measurement, "evidence": evidence,
            "actions": None, "detail": "Advisory local measurement; original evidence retained. No required-check claim."}


def settle_cancellation(cancelled, plan, partition, path, lease, terminal, cleanup):
    """The controller's cancellation a shard consumed (`runner.Cancelled`), after this
    original's owned scratch cleanup: with every cleanup confirmed it carries the original's
    `interrupted` outcome, its sealed partial evidence kept and no later shard started; with
    any unconfirmed it carries none, so the claim turns uncertain and the fence stays."""
    if lease.uncertain:
        add_note(cancelled, "owned shard cleanup is uncertain; retained original fence")
        return cancelled
    try:
        with terminal.enforce("cleaning local scratch after cancellation"):
            problems = cleanup()
    except Exception as error:  # noqa: BLE001 - kept on the cancellation, never swallowed
        problems = ["%s: %s" % (type(error).__name__, error)]
    if problems:
        add_note(cancelled, "scratch cleanup unconfirmed; retained original fence: "
                 + "; ".join(problems)[:2048])
        return cancelled
    index = cancelled.doc["shard"]
    owned = [s["index"] for s in plan["shards"]
             if partition is None or s["index"] in partition["shards"]]
    later = [i for i in owned if i > index]
    with terminal.enforce("sealing cancelled local evidence"):
        evidence = file_manifest(path, EVIDENCE_PREFIXES, terminal)
    cancelled.result = {
        "status": "interrupted", "measurement_status": "interrupted", "evidence": evidence,
        "actions": None, "detail": (
            "cancelled by signal %d during shard %d: its sealed record is kept, shards %s and "
            "every later original were not started; cleanup confirmed. No verdict." % (
                cancelled.signum, index, ", ".join(map(str, later)) or "none"))[:4096]}
    return cancelled


def partition_authority(plan, partition, request_id, builds_bytes, built, started_unix_ms):
    """The `af.perf-partition-execution/1` bytes of a later partition original (ADR-0265): its
    own run window, the plan's frozen total minutes from this execution's start, bound to the
    reused build's exact bytes and its recorded run, which stays unchanged provenance. The
    caller's deadline (this original's `run_seconds`) still tightens every shard."""
    total = plan["limits"]["run_total_minutes"]
    doc = {"schema": "af.perf-partition-execution/1",
           "comparison_id": partition["comparison_id"], "plan_digest": plan["plan_digest"],
           "builds_sha256": sha256_bytes(builds_bytes), "build_from": partition["build_from"],
           "build_run": built["execution"]["run"],
           "partitions": [{"index": partition["index"], "request_id": request_id,
                           "shards": partition["shards"],
                           "run": {"started_unix_ms": started_unix_ms,
                                   "deadline_unix_ms": started_unix_ms + total * 60000,
                                   "total_minutes": total}}]}
    return contracts.write_bytes(doc, "partition-execution")


def partition_build(config, request, installed=ROOT):
    """(build directory, builds.json bytes) of the first partition a later partition reuses:
    a completed, offline-verified original of the same comparison and plan, whose build lies in
    its retained, hash-bound evidence (`sources.git`). Refused otherwise."""
    partition = request["partition"]
    with Journal(config, readonly=True) as journal:
        first, _, _ = verify_original(journal, partition["build_from"], installed)
        require(first["partition"] is not None
                and first["partition"]["comparison_id"] == partition["comparison_id"]
                and first["partition"]["index"] == 0
                and first["plan_sha256"] == request["plan_sha256"]
                and first["sources"] == request["sources"],
                "the reused build belongs to another comparison, plan or source pair")
        root = journal.path(partition["build_from"])
    plan = contracts.read_json(read(os.path.join(root, "plan.json")), "run")
    builds_bytes = read(os.path.join(root, "run", "builds.json"))
    layout = builds.Layout(os.path.join(root, "sources.git"), plan["run_id"])
    require(os.path.isdir(layout.build) and not os.path.islink(layout.build),
            "the first partition retains no build directory")
    return layout.build, builds_bytes


def dispatch_actions(config, request, path, transport=None):
    """The hosted backend delegates to the unchanged workflow; it never executes locally.

    Dispatch itself has no Actions run ID. Its durable intent remains submitted even if
    discovery is delayed; the ordinary hosted collector retains real run/attempt receipts.
    An ambiguous HTTP handoff is uncertain and is never automatically retried.
    """
    require(config["telemetry"]["scale"] is None and config["telemetry"]["corpus_runs"] is None,
            "hosted workflow cannot express scale/corpus overrides; refuse rather than substitute")
    require(request["work"] != "profile", "hosted profiles use the existing explicit diagnostic workflow path")
    workflow = "performance-telemetry.yml" if request["work"] == "telemetry" else "performance.yml"
    sources = request["sources"]
    if request["work"] == "core":
        git = source_repository(config, budget.Budget(
            time.monotonic() + config["cycle_seconds"], time.monotonic, 0))
        suite = planning.read_suite(git, sources["control_sha"])
        default = suite.doc["tiers"][config["core"]["tier"]]["profiles"][0]["environment"]
        require(config["core"]["environment"] == default,
                "the hosted workflow fixes its default tier environment; choose that explicitly")
    inputs = {"base": sources["baseline_sha"], "head": sources["candidate_sha"], "control": sources["control_sha"]}
    if request["work"] == "core":
        inputs.update(scope="partial_scope" if config["core"]["cases"] else "bootstrap",
                      tier=config["core"]["tier"], cases=" ".join(config["core"]["cases"]),
                      shards=str(config["core"]["shards"]), authority=sources["authority_sha"])
    else:
        t = config["telemetry"]
        inputs.update(profile=t["profile"], experiments=" ".join(t["experiments"]), rounds=str(t["rounds"]))
        if t["pairs"] is not None:
            inputs["pairs"] = str(t["pairs"])
    payload = {"ref": config["trusted_ref"].removeprefix("refs/heads/"), "inputs": inputs}
    if transport is None:
        # gh owns credentials only in this controller process; builders use explicit clean
        # environments. The command is bounded and no secret value is retained in a receipt.
        def transport(workflow, payload):
            completed = subprocess.run(["gh", "api", "--method", "POST", "repos/" + config["repository"]
                + "/actions/workflows/" + workflow + "/dispatches", "--input", "-"],
                input=json.dumps(payload).encode(), stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=config["cycle_seconds"], check=True)
            require(len(completed.stdout) <= 2048, "dispatch response exceeds receipt bound")
            return "GitHub accepted workflow dispatch; run identity pending ordinary hosted collection"
    receipt = transport(workflow, payload)
    require(isinstance(receipt, str) and len(receipt) <= 2048, "invalid Actions dispatch receipt")
    return {"status": "submitted", "measurement_status": "pending hosted collection", "evidence": [],
        "actions": {"repository": config["repository"], "workflow": workflow,
                    "control_sha": sources["control_sha"], "requested_ref": config["trusted_ref"],
                    "receipt": receipt},
        "detail": "Hosted dispatch accepted. Only original Actions run/attempt evidence establishes measurement."}


def verify_original(journal, ident, installed=ROOT):
    """Offline admission: local journal, retained Git objects, plan and raw evidence, no API."""
    request, state = journal.request(ident), journal.state(ident)
    require(request["backend"] == "local" and state["state"] == "complete", "not a completed local original")
    path = journal.path(ident)
    config = contracts.read_json(read(os.path.join(path, "configuration.json")), "executor")
    require(canonical_digest(config) == request["configuration_digest"]
            and environment(config) == request["environment_digest"] and config["backend"] == "local",
            "original frozen local configuration changed")
    result = contracts.read_json(read(os.path.join(path, "result.json")), "executor-result")
    require(sha256_bytes(contracts.write_bytes(result, "executor-result")) == state["result_sha256"], "result receipt changed")
    require(result["request_digest"] == canonical_digest(request) and result["sources"] == request["sources"]
            and result["backend"] == "local" and result["environment_digest"] == request["environment_digest"]
            and result["producer_digest"] == request["producer_digest"] and result["actions"] is None,
            "local provenance does not match original reservation")
    require(file_manifest(path, EVIDENCE_PREFIXES) == result["evidence"],
            "original evidence membership or bytes changed")
    for entry in result["evidence"]:
        fd, info = admission.open_confined(path, entry["path"], "local original")
        try:
            require(info.st_size == entry["size"] and admission.read_bounded(fd, info.st_size, None,
                    entry["path"], "verifying original") == entry["sha256"], "local raw evidence changed")
        finally:
            os.close(fd)
    git = gitrefs.Git(os.path.join(path, "sources.git"), budget=budget.Budget(time.monotonic() + 120, time.monotonic, 0))
    for sha in set(request["sources"].values()):
        require(git.resolve(sha, "retained source") == sha, "original source object unavailable")
    require(producer(git, request["sources"]["control_sha"], installed) == request["producer_digest"],
            "install the original trusted producer before admitting local evidence")
    if request["work"] == "core":
        plan = contracts.read_json(read(os.path.join(path, "plan.json")), "run")
        require(sha256_bytes(contracts.write_bytes(plan, "run")) == request["plan_sha256"], "original plan changed")
        owner = ident if request["partition"] is None else request["partition"]["comparison_id"]
        require(plan["run_id"] == "local_" + owner[4:]
                and plan["profile"]["environment"]["runner"] == "dedicated",
                "hosted evidence cannot be reclassified as a local original")
        cohort = (contracts.read_json(read(os.path.join(path, "cohort.json")), "local-cohort-declaration")
                  if request["cohort_digest"] is not None else None)
        sealed = (contracts.read_json(read(os.path.join(path, "parameters.json")), "calibration-parameters")
                  if request["parameters_sha256"] is not None else None)
        require(cohort is None or canonical_digest(cohort) == request["cohort_digest"], "original cohort changed")
        require(sealed is None or sha256_bytes(contracts.write_bytes(sealed, "calibration-parameters"))
                == request["parameters_sha256"], "original parameter seal changed")
        core = config["core"]
        expected = planning.plan(git, planning.Request(trigger="manual", tier=core["tier"],
            scope="calibration" if cohort else ("partial_scope" if core["cases"] else "bootstrap"),
            base=request["sources"]["baseline_sha"], head=request["sources"]["candidate_sha"],
            control=request["sources"]["control_sha"], authority=request["sources"]["authority_sha"],
            cases=core["cases"], environment=core["environment"], build_image=config["build_image"],
            shards=core["shards"], run_id=plan["run_id"], cohort=cohort,
            member=request["member_id"], calibration=sealed))
        require(expected == plan, "original plan is not what its retained source authority freezes")
        evaluation = compare.evaluate(os.path.join(path, "run"))
        require(evaluation.plan == plan, "measurement changed original plan")
        for role, source in contracts.BUILD_ROLES:
            require(sha256_bytes(git.read_file(request["sources"][source], "Cargo.lock")) ==
                    plan["build_inputs"][role]["cargo_lock_sha256"], "original Cargo.lock changed")
    elif request["work"] == "telemetry":
        measured, _, _ = telemetrylab.verify(os.path.join(path, "run"), installed)
        require(measured["result_id"] == "local_" + ident[4:]
                and measured["sources"] == {key: request["sources"][key]
                    for key in ("baseline_sha", "candidate_sha", "authority_sha")},
                "native telemetry result is not this local original")
    elif request["work"] == "profile":
        parent = contracts.read_json(read(os.path.join(path, "plan.json")), "run")
        require(sha256_bytes(contracts.write_bytes(parent, "run")) == request["plan_sha256"],
                "diagnostic parent plan changed")
        if request["parent"] is not None:
            require(sha256_bytes(read(os.path.join(path, PARENT_VERDICT)))
                    == request["parent"]["verdict_sha256"], "the frozen parent verdict changed")
        report.admit_children([os.path.join(path, "profile")], parent)
    return request, state, result


@contextlib.contextmanager
def cancellation():
    previous = {}
    def cancel(signum, frame):
        raise KeyboardInterrupt("local controller cancelled by signal %d" % signum)
    for signum in (signal.SIGTERM, signal.SIGINT):
        previous[signum] = signal.signal(signum, cancel)
    try:
        yield
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
