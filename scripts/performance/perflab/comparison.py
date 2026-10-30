"""One durable native release comparison on the local executor (ADR-0240).

A full comparison of a source pair is declared once, before any original exists
(`af.perf-comparison/1` under `comparisons/<cmp_id>/`): its frozen core plan, the serial
partition of that plan's shards into sequential bounded core originals, the telemetry original
and the trace and CPU profile lanes with the recipe each must use for the plan's target. Every
member is an ordinary journal original, executed by `cycle` under its own unchanged caps, the
machine's one exclusion and its own deadline; nothing here executes candidate code.

- **Partition.** The first core partition builds; every later one runs its shards on that
  partition's verified, immutable build (`executor.partition_build`), so every shard bundle
  binds one build. Each partition must fit one bounded original (`executor.partition_seconds`);
  a plan whose single shard window cannot fit is refused, never relaxed.
- **Aggregate.** Once every partition is terminal, `advance` assembles the declared shards of
  the verified partitions into one run of the frozen plan and evaluates it
  (`af.perf-comparison-aggregate/1`): the whole family's denominator, with a failed or missing
  partition's records missing, never a falsely complete result.
- **Diagnosis.** The aggregate's verdict selects what the trace and CPU profile originals
  diagnose (regressed, then inconclusive cases); a profile's parent is the aggregate itself.
- **Report.** `report` recomputes from durable state and offline verification
  (`af.perf-comparison-report/1`); completeness, verification, the performance verdict and
  calibration eligibility are separate. `export` copies the report and every linked native
  record into a self-contained bundle with a hash manifest (`af.perf-comparison-export/1`),
  which `verify_export` checks after copying, with no journal: it derives the report again
  from the bundled declaration and native records and refuses one that claims otherwise.

Preflight and launch refusals are retained under `comparison-refusals/`. Launch and advance
are resumable: a member is submitted by its declared key, so a resumed launch returns the
existing original and never replays it.
"""
import copy
import html
import os
import posixpath
import re
import shutil
import time
import urllib.parse

from . import (admission, archive, budget, compare, contracts, diagcontracts, diagnostics,
               executor, overview, planning, report, runner, telemetrylab)
from .values import ContractError, canonical_digest, require, sha256_bytes

DIRECTORY = "comparisons"
REFUSALS = "comparison-refusals"
REFUSALS_MAX = 256
LANES = ("core", "telemetry", "trace", "cpu_profile")
DIAGNOSTIC_LANES = ("trace", "cpu_profile")
REPORT_FILES = ("report.json", "report.md", "report.html")
MANIFEST = "manifest.json"
EVIDENCE = "evidence"
EXPORT_ENTRIES_MAX = 16384
# The native evidence of each original an export carries whole: everything its own pages link
# and everything its offline verification reads (its plan, run bundle, report and evidence).
NATIVE = {"core": ("plan.json", "run", "report"), "telemetry": ("run",),
          "profile": ("plan.json", "profile", executor.PARENT_VERDICT)}
# The journal records of an original, outside its sealed evidence.
RECORDS = ("request.json", "state.json", "result.json", "configuration.json")
PAGES = (".html", ".md")
HTML_LINK = re.compile(r"""(?:href|src)\s*=\s*(?:"([^"]*)"|'([^']*)')""", re.I)
MD_LINK = re.compile(r"\]\(<?([^)\s>]+)>?\)")
SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")
KEY_MAX = 200
NOT_CALIBRATION = ("a release comparison is not a calibration member: calibration eligibility "
                   "is decided only by a declared local cohort's readiness")


def comparison_id(submission):
    return "cmp_" + canonical_digest({"comparison": submission})[7:55]


def member_id(kind, key, work):
    """The request ID `Journal._submit` derives for this submission."""
    return "exp_" + canonical_digest({"submission": {"kind": kind, "key": key}, "work": work})[7:55]


def directory(journal, ident):
    contracts.ID.validate(ident, "comparison ID")
    require(ident.startswith("cmp_"), "a comparison ID starts with cmp_")
    return os.path.join(journal.root, DIRECTORY, ident)


def schedule(plan, run_seconds):
    """The serial partition of the plan's shards into bounded sequential originals."""
    limits = plan["limits"]
    build = limits["build_minutes"] * 60
    window = limits["shard_sampling_minutes"] * 60
    reserve = runner.FINISH_RESERVE_SECONDS
    available = executor.available_seconds(plan, run_seconds)
    first = (available - build - reserve) // window
    later = (available - executor.REUSE_SECONDS - reserve) // window
    require(first >= 1 and later >= 1,
            "one %d s shard window after a %d s build (or %d s build reuse) and a %d s finish "
            "reserve does not fit one bounded %d s original: no serial partition covers the "
            "family, and no window, sentinel or deadline is relaxed to fit"
            % (window, build, executor.REUSE_SECONDS, reserve, available))
    indices = [shard["index"] for shard in plan["shards"]]
    chunks = [indices[:first]]
    rest = indices[first:]
    while rest:
        chunks.append(rest[:later])
        rest = rest[later:]
    return {"backend": "local_serial", "shards": len(indices), "build_seconds": build,
            "reuse_seconds": executor.REUSE_SECONDS, "shard_sampling_seconds": window,
            "finish_reserve_seconds": reserve, "available_seconds": available,
            "first_child_shards": min(first, 16), "later_child_shards": min(later, 16),
            "children": len(chunks)}, chunks


def lane_config(config, recipe):
    """A diagnostic lane's derived configuration: its recipe, selected from the aggregate."""
    derived = copy.deepcopy(config)
    derived["diagnostic"].update(recipe=recipe, cases=[])
    return contracts.validate_executor(derived)


def recipes(git, control, plan):
    """{lane: (recipe or None, why unsupported or None)}: the one registry recipe of each
    diagnostic kind built for the plan's target, a musl shipping parent's musl recipes."""
    suite = planning.read_suite(git, control)
    raw = git.read_file(control, diagnostics.REGISTRY_PATH)
    require(raw is not None, "control has no diagnostic registry")
    registry = diagnostics.load_bytes(raw, diagnostics.REGISTRY_PATH, suite.doc)
    target = plan["profile"]["recipe"]
    chosen = {}
    for lane in DIAGNOSTIC_LANES:
        found = sorted(rid for rid, entry in registry["recipes"].items()
                       if entry["kind"] == lane
                       and suite.doc["recipes"][entry["base_recipe"]]["target"] == target["target"]
                       and suite.doc["recipes"][entry["base_recipe"]]["os"] == target["os"])
        require(len(found) <= 1, "more than one %s recipe is built for %s: %s"
                % (lane, target["target"], ", ".join(found)))
        chosen[lane] = ((found[0], None) if found else
                        (None, "no %s recipe of the registry is built for %s" % (
                            lane, target["target"])))
    return chosen, registry


def declare(journal, base, head, key, kind="manual", installed=executor.ROOT, git=None):
    """The `af.perf-comparison/1` declaration of a full comparison, and its frozen plan;
    nothing is written."""
    c = journal.config
    require(c["backend"] == "local", "a native comparison runs on the local backend; Actions "
            "keeps its own parallel workflows and is never a fallback")
    require(not c["core"]["cases"], "a full comparison measures the whole core family; "
            "core.cases selects a partial scope")
    require(0 < len(key) <= KEY_MAX, "a comparison key has 1 to %d characters" % KEY_MAX)
    timing = budget.Budget(time.monotonic() + c["cycle_seconds"], time.monotonic, 0)
    git = git or executor.source_repository(c, timing)
    control = git.resolve(c["control_sha"], "installed control")
    require(control == c["control_sha"], "control must be an exact full SHA")
    trusted = git.exact_ref(c["trusted_ref"], "reviewed main")
    require(trusted is not None and git.is_ancestor(control, trusted),
            "control is not reviewed main authority")
    sources = {"baseline_sha": git.resolve(base, "baseline"),
               "candidate_sha": git.resolve(head, "candidate"),
               "control_sha": control, "authority_sha": control}
    submission = {"kind": kind, "key": key}
    ident = comparison_id(submission)
    core = c["core"]
    plan = planning.plan(git, planning.Request(trigger="manual", tier=core["tier"],
        scope="bootstrap", base=sources["baseline_sha"], head=sources["candidate_sha"],
        control=control, authority=control, cases=[], environment=core["environment"],
        build_image=c["build_image"], shards=core["shards"], run_id="local_" + ident[4:],
        windows=core["windows"]))
    require(not plan["required_check_eligible"]
            and plan["profile"]["environment"]["runner"] == "dedicated"
            and plan["profile"]["environment"]["containment"] == "linux_container",
            "local core needs a dedicated contained environment")
    bounds, chunks = schedule(plan, c["run_seconds"])
    partitions = []
    for index, shards in enumerate(chunks):
        child_key = "%s:core:%d" % (key, index)
        partitions.append({"index": index, "shards": shards, "key": child_key,
                           "request_id": member_id(kind, child_key, "core"),
                           "build_from": None if index == 0 else partitions[0]["request_id"]})
        partitions[-1]["needed_seconds"] = executor.partition_seconds(plan, partitions[-1])
    chosen, registry = recipes(git, control, plan)
    lanes = []
    for lane in DIAGNOSTIC_LANES:
        recipe, why = chosen[lane]
        lane_key = "%s:%s" % (key, lane)
        lanes.append({"lane": lane, "recipe": recipe,
                      "observer": (executor.observed(c, registry, recipe) if recipe else None),
                      "key": lane_key,
                      "request_id": member_id(kind, lane_key, "profile") if recipe else None,
                      "unsupported": why})
    telemetry_key = key + ":telemetry"
    doc = {"schema": "af.perf-comparison/1", "comparison_id": ident,
           "journal_id": c["journal_id"], "machine_id": c["machine_id"],
           "submission": submission, "sources": sources,
           "configuration_digest": canonical_digest(c),
           "environment_digest": executor.environment(c),
           "producer_digest": executor.producer(git, control, installed),
           "created_unix_ms": int(time.time() * 1000),
           "plan_sha256": sha256_bytes(contracts.write_bytes(plan, "run")),
           "run_id": plan["run_id"], "lanes": list(LANES), "schedule": bounds,
           "partitions": partitions,
           "telemetry": {"key": telemetry_key,
                         "request_id": member_id(kind, telemetry_key, "telemetry")},
           "diagnostics": lanes}
    return contracts.CONTRACTS["comparison"][2](doc), plan, registry, git


def _same(declared, retained):
    return dict(declared, created_unix_ms=0) == dict(retained, created_unix_ms=0)


def _partition(doc, item):
    """The partition record a core member of `doc` carries."""
    return {"comparison_id": doc["comparison_id"], "comparison_digest": canonical_digest(doc),
            "index": item["index"], "shards": item["shards"], "build_from": item["build_from"]}


def _configuration(journal, doc, lane=None):
    """The digest of the frozen configuration a member of `doc` runs under: the declared one,
    or for a diagnostic lane its derivation from it. A journal whose configuration is no longer
    the declared one derives no lane: None."""
    if lane is None:
        return doc["configuration_digest"]
    if canonical_digest(journal.config) != doc["configuration_digest"]:
        return None
    return canonical_digest(lane_config(journal.config, lane["recipe"]))


def _binding(journal, doc, request, work, key, partition=None, lane=None):
    """Why the retained original `request` is not the member `doc` declares, or None. A member
    is bound to the comparison's sources, submission, partition, frozen configuration and
    declared workload (the frozen plan of a core partition): a same-key original of a narrower
    configuration (another telemetry profile or experiment set, partial core cases) answers
    another question, and its own run list is never this comparison's coverage."""
    if request["sources"] != doc["sources"] or request["work"] != work or request[
            "submission"] != {"kind": doc["submission"]["kind"], "key": key}:
        return "it was submitted for other sources, work or key"
    if request["schema"] != contracts.EXECUTOR_REQUEST_SCHEMA:
        # An earlier request version (ADR-0266) froze no partition or parent.
        return "it is an %s original, never a comparison member" % request["schema"]
    if work != "profile" and request["partition"] != partition:
        return "it is not this comparison's partition"
    expected = _configuration(journal, doc, lane)
    if expected is None or request["configuration_digest"] != expected:
        return ("its frozen configuration %s is not the comparison's %s: another declared "
                "workload" % (request["configuration_digest"], expected))
    if work == "core" and request["plan_sha256"] != doc["plan_sha256"]:
        return "it runs another plan than the comparison's frozen plan"
    if work == "telemetry" and request["plan_sha256"] is not None:
        return "a telemetry original carries no plan"
    return None


def _checks(journal, doc, plan, registry, git, installed):
    """Every member's submission check, as the executing account, reserving nothing; the
    host observer's actual capability for the CPU lane. Returns (checks, refusal or None)."""
    checks = []
    c = journal.config
    base, head = doc["sources"]["baseline_sha"], doc["sources"]["candidate_sha"]
    kind = doc["submission"]["kind"]
    names = journal.names()
    pending = [item["request_id"] for item in doc["partitions"]] + [doc["telemetry"]["request_id"]]
    new = [ident for ident in pending if ident not in names]
    queued = sum(journal.state(i)["state"] == "queued" for i in names)
    profiles = sum(1 for lane in doc["diagnostics"] if lane["request_id"])

    def check(name, action):
        try:
            detail = action() or ""
        except (ContractError, OSError) as error:
            checks.append({"name": name, "state": "refused", "detail": str(error)[:4096]})
            return False
        checks.append({"name": name, "state": "passed", "detail": detail[:4096]})
        return True

    def capacity():
        require(queued + len(new) <= c["max_queue"], "the queue cannot hold this comparison's "
                "%d partitions and telemetry original beside %d queued" % (len(new), queued))
        require(len(names) + len(new) + profiles <= c["max_requests"],
                "the journal cannot hold every original of this comparison")
        return "%d originals to submit now, %d profiles after the aggregate" % (len(new), profiles)

    def member(ident, work, key, partition=None, frozen_plan=None):
        if ident in names:
            # A resumed launch: the retained original must be exactly this member.
            why = _binding(journal, doc, journal.request(ident), work, key, partition)
            require(why is None, "original %s is not this comparison's member: %s; nothing is "
                    "claimed or built" % (ident, why))
            return "already submitted: " + journal.state(ident)["state"]
        checked = {"serial": None, "tools": []}
        journal._submit(work, base, head, key, kind, git=git, installed=installed,
                        checked=checked, frozen_plan=frozen_plan, partition=partition)
        # The same time and storage admission its execution applies, with its figures.
        return checked.get("admission", "")

    steps = [("paths", lambda: [executor.reachable(c[k], k) for k in ("root", "repository_path")]
              and None), ("capacity", capacity)]
    for item in doc["partitions"]:
        partition = _partition(doc, item)
        steps.append(("core_%d" % item["index"], lambda partition=partition, item=item: (
            "; ".join(text for text in (
                member(item["request_id"], "core", item["key"], partition, plan),
                "shards %s within %d of %d s" % (item["shards"], item["needed_seconds"],
                                                 doc["schedule"]["available_seconds"])) if text))))
    steps.append(("telemetry", lambda: member(doc["telemetry"]["request_id"], "telemetry",
                                              doc["telemetry"]["key"])))
    for lane in doc["diagnostics"]:
        def diagnostic(lane=lane):
            require(lane["unsupported"] is None, "the %s lane is unsupported: %s"
                    % (lane["lane"], lane["unsupported"]))
            derived = lane_config(c, lane["recipe"])
            records, refusal = executor.diagnostic_tools(derived, registry, lane["recipe"],
                                                         diagnostics.LINUX_CONTAINER)
            require(refusal is None, "profile refused before any build: " + str(refusal))
            if lane["observer"] is not None:
                record, refusal = journal.capability(derived, registry, lane["recipe"])
                require(refusal is None, "profile refused before any build: " + str(refusal))
                return "%s with %s; %s" % (lane["recipe"], lane["observer"], record["detail"])
            return "%s; tools %s" % (lane["recipe"], ", ".join(
                "%s %s" % (r["name"], r["state"]) for r in records) or "none")
        steps.append((lane["lane"], diagnostic))
    refusal = None
    for name, action in steps:
        if not check(name, action) and refusal is None:
            refusal = "%s: %s" % (name, checks[-1]["detail"])
    return checks, refusal


def _retain_refusal(journal, doc):
    root = os.path.join(journal.root, REFUSALS)
    executor.private_directory(root, create=True)
    names = os.listdir(root)
    require(len(names) < REFUSALS_MAX, "the retained comparison refusals are at their bound; "
            "inspect and archive them before another attempt")
    name = "%s-%d-%s.json" % (doc["operation"], doc["unix_ms"], os.urandom(4).hex())
    executor.atomic(os.path.join(root, name), doc, "comparison-preflight")
    return os.path.join(REFUSALS, name)


def preflight(journal, base, head, key, kind="manual", installed=executor.ROOT, launch=False,
              git=None):
    """`af.perf-comparison-preflight/1`: the declaration and every member's check, `ready`
    (or, with `launch`, `launched` after submitting the core partitions and telemetry), or
    `refused` with the first refusal, which is retained under `comparison-refusals/`."""
    submission = {"kind": kind, "key": key}
    doc = {"schema": "af.perf-comparison-preflight/1",
           "operation": "launch" if launch else "preflight", "status": "refused",
           "detail": "", "unix_ms": int(time.time() * 1000), "submission": submission,
           "comparison": None, "checks": []}
    declared = None
    try:
        declared, plan, registry, git = declare(journal, base, head, key, kind, installed, git)
        path = directory(journal, declared["comparison_id"])
        if os.path.lexists(os.path.join(path, "comparison.json")):
            retained = contracts.read_json(executor.read(os.path.join(path, "comparison.json")),
                                           "comparison")
            require(_same(declared, retained), "the comparison key was declared for another "
                    "source pair, plan or configuration")
            declared = retained
        checks, refusal = _checks(journal, declared, plan, registry, git, installed)
        doc["checks"] = checks
        require(refusal is None, refusal or "")
        if launch:
            _launch(journal, declared, plan, git, installed)
            doc["status"] = "launched"
        else:
            doc["status"] = "ready"
        doc["comparison"] = declared
    except ContractError as error:
        if isinstance(error, executor.JournalBusy):
            raise
        doc.update(status="refused", detail=str(error)[:4096] or "refused", comparison=None)
        doc = contracts.CONTRACTS["comparison-preflight"][2](doc)
        _retain_refusal(journal, doc)
        return doc
    return contracts.CONTRACTS["comparison-preflight"][2](doc)


def _launch(journal, doc, plan, git, installed):
    """Persist the declaration and frozen plan, then submit the partitions in order and the
    telemetry original; resumable, never a replay."""
    root = os.path.join(journal.root, DIRECTORY)
    executor.private_directory(root, create=True)
    path = directory(journal, doc["comparison_id"])
    executor.private_directory(path, create=True)
    target = os.path.join(path, "comparison.json")
    if not os.path.lexists(target):
        executor.atomic(os.path.join(path, "plan.json"), plan, "run")
        executor.atomic(target, doc, "comparison")
    base, head = doc["sources"]["baseline_sha"], doc["sources"]["candidate_sha"]
    kind = doc["submission"]["kind"]
    for item in doc["partitions"]:
        partition = _partition(doc, item)
        request = journal.submit("core", base, head, item["key"], kind, git=git,
                                 installed=installed, frozen_plan=plan, partition=partition,
                                 bind_configuration=True)
        require(request["request_id"] == item["request_id"], "partition identity changed")
    request = journal.submit("telemetry", base, head, doc["telemetry"]["key"], kind, git=git,
                             installed=installed, bind_configuration=True)
    require(request["request_id"] == doc["telemetry"]["request_id"], "telemetry identity changed")


def load(journal, ident):
    """The retained declaration and frozen plan, each checked against the other."""
    path = directory(journal, ident)
    doc = contracts.read_json(executor.read(os.path.join(path, "comparison.json")), "comparison")
    require(doc["comparison_id"] == ident and doc["journal_id"] == journal.config["journal_id"],
            "the comparison belongs to another journal")
    raw = executor.read(os.path.join(path, "plan.json"))
    require(sha256_bytes(raw) == doc["plan_sha256"], "the comparison's frozen plan changed")
    return doc, contracts.read_json(raw, "run"), raw


def _state(journal, ident):
    if ident is None or ident not in journal.names():
        return None
    return journal.state(ident)


def read_aggregate(journal, doc):
    path = os.path.join(directory(journal, doc["comparison_id"]), "aggregate")
    if not os.path.lexists(os.path.join(path, "aggregate.json")):
        return None, None
    raw = executor.read(os.path.join(path, "aggregate.json"))
    agg = contracts.read_json(raw, "comparison-aggregate")
    require(agg["comparison_digest"] == canonical_digest(doc), "the aggregate is of another "
            "comparison declaration")
    if agg["verdict_sha256"] is not None:
        verdict = executor.read(os.path.join(path, "report", report.VERDICT_JSON))
        require(sha256_bytes(verdict) == agg["verdict_sha256"], "the aggregate verdict changed")
    return agg, raw


def aggregate(journal, doc, plan, raw_plan, installed=executor.ROOT):
    """Assemble and evaluate the whole family once every partition is terminal; the
    retained aggregate, or None while a partition is unfinished."""
    agg, _ = read_aggregate(journal, doc)
    if agg is not None:
        return agg
    entries, windows = [], []
    for item in doc["partitions"]:
        state = _state(journal, item["request_id"])
        if state is None or state["state"] not in executor.TERMINAL:
            return None
        entry = {"index": item["index"], "request_id": item["request_id"],
                 "state": state["state"], "verification": "not_complete",
                 "result_sha256": state["result_sha256"], "shards": item["shards"],
                 "included": False, "detail": state["detail"][:4096]}
        why = _binding(journal, doc, journal.request(item["request_id"]), "core", item["key"],
                       _partition(doc, item))
        if why is not None:
            # Another workload's original under the partition's key: its records are missing.
            entry["detail"] = ("not this comparison's member: " + why)[:4096]
            if state["state"] == "complete":
                entry["verification"] = "refused"
        elif state["state"] == "complete":
            try:
                executor.verify_original(journal, item["request_id"], installed)
                windows.append(_window(journal, doc, item))
                entry.update(verification="verified", included=True, detail="")
            except (ContractError, OSError) as error:
                entry.update(verification="refused", detail=str(error)[:4096])
        entries.append(entry)
    first = entries[0]
    path = os.path.join(directory(journal, doc["comparison_id"]), "aggregate")
    work = path + ".pending"
    if os.path.lexists(work):
        # The controller's own derived assembly, never an original: rebuilt from originals.
        problems = admission.remove_tree(work)
        require(not problems, "; ".join(problems))
    os.mkdir(work, 0o700)
    result = {"schema": "af.perf-comparison-aggregate/1", "comparison_id": doc["comparison_id"],
              "comparison_digest": canonical_digest(doc), "plan_sha256": doc["plan_sha256"],
              "builds_sha256": None, "partitions": entries, "verdict_sha256": None,
              "result": None, "coverage": None, "regressions": [],
              "created_unix_ms": int(time.time() * 1000)}
    if first["included"]:
        first_builds = executor.read(os.path.join(journal.path(first["request_id"]), "run",
                                                  "builds.json"))
        run = os.path.join(work, "run")
        # Every included later partition's verified execution authority, in one document, so
        # the family's evaluation checks each shard against its own partition's window.
        later = [window for window in windows if window is not None]
        authority = None
        if later:
            authority = dict(later[0], partitions=[item for window in later
                                                   for item in window["partitions"]])
            authority = contracts.write_bytes(authority, "partition-execution")
        runner.prepare_run_dir(run, raw_plan, first_builds, authority)
        for entry in entries:
            if not entry["included"]:
                continue
            for index in entry["shards"]:
                source = os.path.join(journal.path(entry["request_id"]), "run", "shards",
                                      str(index))
                if os.path.isdir(source) and not os.path.islink(source):
                    shutil.copytree(source, os.path.join(run, "shards", str(index)),
                                    symlinks=True)
        evaluation = compare.evaluate(run)
        report.write(run, os.path.join(work, "report"))
        verdict_bytes = executor.read(os.path.join(work, "report", report.VERDICT_JSON))
        coverage, performance = overview._core(work)
        result.update(builds_sha256=sha256_bytes(first_builds),
                      verdict_sha256=sha256_bytes(verdict_bytes),
                      result=evaluation.verdict["aggregate"]["result"], coverage=coverage,
                      regressions=performance["regressions"])
    executor.atomic(os.path.join(work, "aggregate.json"), result, "comparison-aggregate")
    os.rename(work, path)
    executor.sync_dir(os.path.dirname(path))
    if result["verdict_sha256"] is not None:
        archive_root = os.path.join(journal.root, "core-archive")
        if not os.path.exists(archive_root):
            archive.Archive.create(archive_root, journal.config["archive_bytes"])
        with archive.Archive(archive_root) as store:
            if plan["run_id"] not in store.state():
                store.import_run(os.path.join(path, "run"))
            store.retain()
    return result


def _window(journal, doc, item):
    """The execution authority a verified later partition original sealed in its run bundle,
    exactly this comparison's declared partition on its first partition's build; None for the
    first partition, which runs on its own build's run. Refused when it is missing or names
    another comparison, original, index or shard set."""
    path = os.path.join(journal.path(item["request_id"]), "run", runner.PARTITIONS_FILE)
    if item["build_from"] is None:
        require(not os.path.lexists(path), "the first partition runs on its own build's run "
                "and carries no partition authority")
        return None
    window = contracts.read_json(executor.read(path), "partition-execution", path)
    first = executor.read(os.path.join(journal.path(item["build_from"]), "run", "builds.json"))
    require(window["comparison_id"] == doc["comparison_id"]
            and window["build_from"] == item["build_from"]
            and window["builds_sha256"] == sha256_bytes(first)
            and window["plan_digest"] == contracts.read_json(executor.read(os.path.join(
                journal.path(item["build_from"]), "plan.json")), "run")["plan_digest"]
            and window["build_run"] == contracts.read_json(first, "builds")["execution"]["run"]
            and [(p["index"], p["request_id"], p["shards"]) for p in window["partitions"]]
            == [(item["index"], item["request_id"], item["shards"])],
            "partition %d's execution authority is not its declared partition of %s"
            % (item["index"], doc["comparison_id"]))
    return window


def _parent(doc, agg_raw, raw_plan, verdict_bytes):
    return {"request_id": doc["comparison_id"], "request_digest": canonical_digest(doc),
            "result_sha256": sha256_bytes(agg_raw), "plan_sha256": sha256_bytes(raw_plan),
            "verdict_sha256": sha256_bytes(verdict_bytes)}


def selection(journal, doc, plan, agg):
    """{lane: (selection record, cases)} the aggregate verdict selects for each lane."""
    path = os.path.join(directory(journal, doc["comparison_id"]), "aggregate", "report",
                        report.VERDICT_JSON)
    raw = executor.read(path)
    verdict = contracts.read_json(raw, "verdict")
    return diagnostics.select(plan, verdict, (), journal.config["diagnostic"]["max_cases"],
                              sha256_bytes(raw)), raw


def advance(journal, ident, installed=executor.ROOT, git=None):
    """Aggregate a finished core family and submit the diagnostic lanes it selects; the
    comparison's current report (unverified). Resumable and idempotent."""
    doc, plan, raw_plan = load(journal, ident)
    agg = aggregate(journal, doc, plan, raw_plan, installed)
    if agg is not None and agg["verdict_sha256"] is not None:
        (record, cases), verdict_bytes = selection(journal, doc, plan, agg)
        _, agg_raw = read_aggregate(journal, doc)
        if cases:
            require(canonical_digest(journal.config) == doc["configuration_digest"],
                    "the journal's configuration is no longer the comparison's frozen "
                    "configuration: no diagnostic lane is derived from another")
            base, head = doc["sources"]["baseline_sha"], doc["sources"]["candidate_sha"]
            for lane in doc["diagnostics"]:
                if lane["request_id"] is None:
                    continue
                request = journal.submit(
                    "profile", base, head, lane["key"], doc["submission"]["kind"], git=git,
                    installed=installed, config=lane_config(journal.config, lane["recipe"]),
                    frozen_parent=(_parent(doc, agg_raw, raw_plan, verdict_bytes), plan,
                                   verdict_bytes), bind_configuration=True)
                require(request["request_id"] == lane["request_id"], "profile identity changed")
    return status(journal, ident, verify=None, installed=installed)


def _trace_side(doc, instance, side):
    """Why the profile holds no complete `side` trace of `instance`, or None: an admitted trace
    without omission whose region coverage is complete (a partial or setup-only trace is never
    complete, ADR-0176)."""
    found = [item for item in doc["traces"]
             if item["instance_id"] == instance and item["side"] == side]
    if any(item["omission"] is None and item["coverage"] == "complete" for item in found):
        return None
    if not found:
        return "no %s trace" % side
    item = found[0]
    if item["omission"] is not None:
        return "its %s trace omitted (%s: %s)" % (side, item["omission"]["reason"],
                                                  item["omission"]["detail"][:256])
    return "a %s trace of %s coverage (%d %s spans, %d dropped, %d open)" % (
        side, item["coverage"], item["region_spans"], item["region_phase"], item["dropped"],
        item["open"])


def _minimum_samples(doc, installed):
    """The frozen registry's `capture.minimum_samples` the profile was captured under, read from
    the installed trusted producer whose registry bytes are exactly the ones the profile names
    (`registry_sha256`); (None, why) when they are not."""
    try:
        raw = executor.read(os.path.join(installed, diagnostics.REGISTRY_PATH))
        if sha256_bytes(raw) != doc["registry_sha256"]:
            return None, ("the profile's diagnostic registry %s is not the installed "
                          "producer's %s, so its frozen sample minimum is unknown; install the "
                          "original trusted producer" % (doc["registry_sha256"],
                                                         sha256_bytes(raw)))
        return diagnostics.load_bytes(raw)["capture"]["minimum_samples"], None
    except (ContractError, OSError) as error:
        return None, "the installed diagnostic registry is unreadable: %s" % str(error)[:512]


def _cpu_side(doc, instance, side, minimum):
    """({digest of the collapsed stacks of each usable `side` capture of `instance`}, why none
    is usable): a capture is usable exactly when it has no omission, at least the frozen
    registry's minimum samples and its own collapsed stacks, the input a flamegraph renders
    (`diagnostics.Profile.render`)."""
    found = [item for item in doc["captures"]
             if item["instance_id"] == instance and item["side"] == side]
    usable, why = set(), []
    for item in found:
        folded = [artifact["sha256"] for artifact in item["artifacts"] if artifact["path"]
                  == diagcontracts.capture_directory(item) + "/collapsed.folded"]
        if item["omission"] is not None:
            why.append("its %s capture %d omitted (%s: %s)" % (
                side, item["attempt"], item["omission"]["reason"],
                item["omission"]["detail"][:256]))
        elif item["samples"] is None or item["samples"] < minimum:
            why.append("a %s capture %d of %s samples, below the frozen minimum of %d" % (
                side, item["attempt"], item["samples"], minimum))
        elif not folded:
            why.append("a %s capture %d without its collapsed stacks" % (side, item["attempt"]))
        else:
            usable.add(folded[0])
    return usable, "; ".join(why) or "no %s capture" % side


def _cpu_case(doc, instance, minimum):
    """Why the profile holds no usable differential flamegraph of `instance`, or None: a usable
    baseline and candidate capture each, and both directions of the difference rendered from
    exactly those captures' collapsed stacks (the forward one baseline then candidate)."""
    usable, missing = {}, []
    for side in diagcontracts.SIDES:
        usable[side], why = _cpu_side(doc, instance, side, minimum)
        if not usable[side]:
            missing.append(why)
    if missing:
        return "no usable two-sided capture: " + "; ".join(missing)
    inputs = dict((item["kind"], item["inputs"]) for item in doc["renders"]
                  if item["instance_id"] == instance and item["kind"].startswith("difference"))
    if not inputs:
        return "no differential flamegraph"
    forward, reverse = inputs.get("difference_forward"), inputs.get("difference_reverse")
    if (forward is None or reverse != forward[::-1] or forward[0] not in usable["baseline"]
            or forward[1] not in usable["candidate"]):
        return ("a differential flamegraph whose inputs %s are not its usable baseline and "
                "candidate captures' collapsed stacks" % (forward or reverse))
    return None


def _profile_usable(path, lane, expected, installed=executor.ROOT):
    """Reasons the profile's evidence is not usable for the cases the aggregate verdict selects.
    `expected` is the selection record `selection` derives from the bundled aggregate: the
    profile's own selection must be exactly that record (mode, limit, parent verdict digest,
    cases and reasons), never a self-declared one, and each expected case needs a complete
    baseline and candidate trace (trace) or a differential flamegraph rendered from a usable
    baseline and candidate capture under the frozen registry's sample minimum (CPU,
    `_cpu_case`), never a surviving render alone."""
    doc = contracts.read_file(os.path.join(path, "profile", diagnostics.PROFILES_FILE),
                              "profiles")
    reasons = []
    minimum = None
    if lane == "cpu_profile" and expected["cases"]:
        minimum, why = _minimum_samples(doc, installed)
        if why is not None:
            reasons.append("%s lane: %s" % (lane, why))
    own = doc["selection"]
    if own != expected:
        reasons.append("%s lane: the profile selected %s %s under verdict %s, not the aggregate "
                       "verdict's %s %s under %s" % (
                           lane, own["mode"], [c["instance_id"] for c in own["cases"]][:8],
                           own["parent_verdict_sha256"], expected["mode"],
                           [c["instance_id"] for c in expected["cases"]][:8],
                           expected["parent_verdict_sha256"]))
    for instance in [case["instance_id"] for case in expected["cases"]]:
        if lane == "cpu_profile":
            why = None if minimum is None else _cpu_case(doc, instance, minimum)
            if why is not None:
                reasons.append("%s lane: case %s has %s" % (lane, instance, why))
        else:
            missing = [why for why in (_trace_side(doc, instance, side)
                                       for side in diagcontracts.SIDES) if why]
            if missing:
                reasons.append("%s lane: case %s has no complete two-sided trace: %s"
                               % (lane, instance, "; ".join(missing)))
    return reasons, doc


def status(journal, ident, verify=None, installed=executor.ROOT):
    """The `af.perf-comparison-report/1` of durable state. `verify` (`executor.verify_original`)
    recomputes every complete original's admission, and the aggregate's claims are derived
    again from the admitted partitions and its re-evaluated verdict (`aggregate_claims`), so a
    journal's report and a copied bundle's agree; without it nothing is verified and the
    comparison is never complete (`status` reads cheaply while an original executes)."""
    doc, plan, raw_plan = load(journal, ident)
    root = journal.root
    agg, agg_raw = read_aggregate(journal, doc)
    base = os.path.relpath(directory(journal, ident), root)
    members, reasons, lanes, picks, links = [], [], [], [], []
    verified = {}

    def member(ident, lane, work, key, partition=None, declared=None):
        state = _state(journal, ident)
        record = {"request_id": ident, "lane": lane, "work": work, "key": key,
                  "state": "not_submitted" if state is None else state["state"],
                  "state_detail": "" if state is None else state["detail"],
                  "verification": "not_terminal", "result_sha256": None,
                  "measurement_status": None}
        why = None if state is None else _binding(journal, doc, journal.request(ident), work,
                                                  key, partition, declared)
        if why is not None:
            # Another workload's original under this member's key: never its coverage.
            record["verification"] = "refused"
            reasons.append("%s original %s is not this comparison's member: %s"
                           % (lane, ident, why))
        elif state is not None and state["state"] == "complete":
            record["result_sha256"] = state["result_sha256"]
            if verify is None:
                record["verification"] = "not_checked"
            else:
                try:
                    _, _, result = verify(journal, ident, installed)
                    record["verification"] = "verified"
                    record["measurement_status"] = result["measurement_status"]
                    verified[ident] = result
                except (ContractError, OSError) as error:
                    record["verification"] = "refused"
                    reasons.append("%s original %s evidence refused: %s"
                                   % (lane, ident, str(error)[:512]))
        members.append(record)
        return record

    def lane_state(records):
        states = [r["state"] for r in records]
        if any(s == "not_submitted" for s in states):
            return "waiting"
        if all(s == "queued" for s in states):
            return "queued"
        if any(s in ("queued", "claimed") for s in states):
            return "running"
        return None

    # Core: the partitions, then the aggregate's whole-family judgment.
    core = [member(item["request_id"], "core", "core", item["key"], _partition(doc, item))
            for item in doc["partitions"]]
    state = lane_state(core)
    coverage, detail, core_links = None, "", []
    refused = None
    if verify is not None and agg is not None:
        # A verified report takes no claim of the aggregate on trust: its membership, run,
        # result, coverage and regressions are derived again from the admitted partitions
        # and its re-evaluated verdict, in a journal and a copied bundle alike.
        try:
            aggregate_claims(journal, doc, agg, set(verified))
        except (ContractError, OSError) as error:
            # Journal-relative, so a journal's report and its bundle's say the same.
            refused = "core aggregate refused: %s" % str(error).replace(
                journal.root + os.sep, "")[:1024]
    if state is None:
        if agg is None:
            state, detail = "running", "every partition ended; awaiting the aggregate (advance)"
        elif refused is not None:
            state, detail = "failed", refused
            reasons.append(refused)
        else:
            coverage = agg["coverage"]
            core_links = [os.path.join(base, "aggregate", name) for name in (
                "aggregate.json", "report/report.html", "report/verdict.json")
                if os.path.isfile(os.path.join(root, base, "aggregate", name))]
            missing = [p for p in agg["partitions"] if not p["included"]]
            for p in missing:
                reasons.append("core partition %d (%s) is %s, %s: its shards %s are missing "
                               "records, not passed" % (p["index"], p["request_id"], p["state"],
                                                        p["verification"], p["shards"]))
            if agg["result"] is None:
                state, detail = "failed", "no verified build: nothing of the family was judged"
                reasons.append("core: " + detail)
            elif agg["result"] == "incomplete" or missing:
                state = "failed"
                detail = "the aggregate is %s over %d of %d records" % (
                    agg["result"], coverage["observed"], coverage["required"])
                reasons.append("core aggregate result is %s: its coverage, health or integrity "
                               "reasons are in its report" % agg["result"])
            else:
                state = "complete"
                detail = "the whole family judged: %s" % agg["result"]
            if coverage is not None and (coverage["missing"] or coverage["failed"]):
                reasons.append("core coverage: %d of %d observed, %d failed, %d missing" % (
                    coverage["observed"], coverage["required"], coverage["failed"],
                    coverage["missing"]))
    else:
        detail = "partitions: " + ", ".join("%d %s" % (i, r["state"]) for i, r in enumerate(core))
    lanes.append({"lane": "core", "state": state, "members": [r["request_id"] for r in core],
                  "coverage": coverage, "detail": detail[:4096], "links": core_links})

    # Telemetry.
    tel = member(doc["telemetry"]["request_id"], "telemetry", "telemetry", doc["telemetry"]["key"])
    state = lane_state([tel])
    coverage, detail, tel_links, telemetry_verdict = None, "", [], None
    if state is None:
        path = journal.path(tel["request_id"])
        if tel["state"] != "complete":
            state, detail = "failed", "telemetry original is %s: %s" % (tel["state"],
                                                                         tel["state_detail"])
        elif tel["verification"] == "refused":
            state, detail = "failed", "telemetry evidence refused"
        elif tel["verification"] == "not_checked":
            state, detail = "complete", "complete; its evidence is not verified by status"
        else:
            coverage, performance, result_status = overview._telemetry(path)
            telemetry_verdict = performance["verdict"]
            tel_links = [os.path.relpath(os.path.join(path, name), root)
                         for name in ("run/report.html", "run/result.json")
                         if os.path.isfile(os.path.join(path, name))]
            if result_status != "complete" or coverage["failed"] or coverage["missing"]:
                state = "failed"
                detail = "telemetry result is %s: %d of %d runs, %d failed, %d not run" % (
                    result_status, coverage["observed"], coverage["required"],
                    coverage["failed"], coverage["missing"])
            else:
                state, detail = "complete", "every required telemetry run measured"
        if state == "failed":
            reasons.append(detail)
    lanes.append({"lane": "telemetry", "state": state, "members": [tel["request_id"]],
                  "coverage": coverage, "detail": detail[:4096], "links": tel_links})

    # Diagnostic lanes, driven by the aggregate's verdict.
    chosen = parent = None
    if agg is not None and agg["verdict_sha256"] is not None:
        (record, cases), verdict_bytes = selection(journal, doc, plan, agg)
        chosen, parent = record, _parent(doc, agg_raw, raw_plan, verdict_bytes)
    for lane in doc["diagnostics"]:
        entry = {"lane": lane["lane"], "state": "waiting", "members": [], "coverage": None,
                 "detail": "", "links": []}
        if lane["unsupported"] is not None:
            entry.update(state="unsupported", detail=lane["unsupported"])
            reasons.append("%s lane unsupported: %s" % (lane["lane"], lane["unsupported"]))
        elif agg is None:
            entry["detail"] = "waiting for the core aggregate"
        elif chosen is None:
            entry.update(state="failed", detail="no verified core aggregate to diagnose")
            reasons.append("%s lane: no verified core aggregate to diagnose" % lane["lane"])
        elif not chosen["cases"]:
            entry.update(state="not_applicable", detail="the aggregate verdict has no "
                         "regressed or inconclusive case: nothing to diagnose")
        else:
            for case in chosen["cases"]:
                picks.append({"lane": lane["lane"], "instance_id": case["instance_id"],
                              "selected": True, "reason": case["reason"]})
            for case in chosen["not_selected"]:
                picks.append({"lane": lane["lane"], "instance_id": case["instance_id"],
                              "selected": False, "reason": case["reason"]})
            prof = member(lane["request_id"], lane["lane"], "profile", lane["key"],
                          declared=lane)
            if prof["state"] != "not_submitted" and prof["verification"] != "refused" \
                    and journal.request(prof["request_id"])["parent"] != parent:
                # Diagnosed under another parent receipt: never this aggregate's diagnosis.
                prof["verification"] = "refused"
                reasons.append("%s original %s is not this comparison's member: its parent "
                               "receipt is not the aggregate's result, plan and verdict"
                               % (lane["lane"], prof["request_id"]))
            entry["members"] = [prof["request_id"]]
            waiting = lane_state([prof])
            if waiting is not None:
                entry.update(state=waiting, detail="profile original is " + prof["state"])
            elif prof["state"] != "complete" or prof["verification"] == "refused":
                entry.update(state="failed", detail="profile original is %s %s: %s" % (
                    prof["state"], prof["verification"], prof["state_detail"]))
                reasons.append("%s lane: %s" % (lane["lane"], entry["detail"]))
            else:
                path = journal.path(prof["request_id"])
                result = verified.get(prof["request_id"])
                measured = None if result is None else result["measurement_status"]
                if os.path.isfile(os.path.join(path, "profile", diagnostics.PROFILES_FILE)):
                    unusable, _ = _profile_usable(path, lane["lane"], chosen, installed)
                else:
                    unusable = ["%s lane: the profile retained no profiles.json" % lane["lane"]]
                entry["links"] = [os.path.relpath(os.path.join(path, name), root)
                                  for name in ("profile/diagnostic.html", "profile/profiles.json",
                                               "parent-verdict.json")
                                  if os.path.isfile(os.path.join(path, name))]
                if measured not in (None, "available"):
                    unusable.insert(0, "%s lane: profile measurement is %s" % (lane["lane"],
                                                                               measured))
                if refused is not None:
                    unusable.append("%s lane: its parent aggregate's claims are refused"
                                    % lane["lane"])
                if unusable:
                    entry.update(state="failed", detail="; ".join(unusable))
                    reasons.extend(unusable)
                else:
                    entry.update(state="complete", detail="usable %s evidence for every "
                                 "selected case" % lane["lane"])
        lanes.append(entry)

    for entry in lanes:
        links.extend(entry["links"])
    terminal = all(m["state"] in executor.TERMINAL for m in members)
    states = [m["verification"] for m in members if m["state"] == "complete"]
    if not terminal:
        verification = "not_terminal"
    elif "refused" in states or refused is not None:
        verification = "refused"
    elif verify is None:
        verification = "not_checked"
    else:
        verification = "verified"
    unfinished = any(entry["state"] in ("waiting", "queued", "running") for entry in lanes)
    if verification == "not_checked" and not unfinished:
        reasons.append("status does not verify evidence: `comparison-report` decides "
                       "completeness")
    if unfinished:
        reasons.insert(0, "unfinished lanes: " + ", ".join(
            "%s %s" % (entry["lane"], entry["state"]) for entry in lanes
            if entry["state"] in ("waiting", "queued", "running")))
        status_value = "running"
    elif reasons:
        status_value = "incomplete"
    else:
        status_value = "complete"
    out = {"schema": "af.perf-comparison-report/1", "comparison_id": ident,
           "comparison_digest": canonical_digest(doc), "journal_id": doc["journal_id"],
           "machine_id": doc["machine_id"], "sources": doc["sources"],
           "submission": doc["submission"], "status": status_value,
           "verification": verification,
           "calibration": {"eligible": False, "detail": NOT_CALIBRATION},
           "performance": {"core_verdict": (None if agg is None or refused is not None
                                            else agg["result"]),
                           "regressions": ([] if agg is None or refused is not None
                                           else agg["regressions"]),
                           "telemetry_verdict": telemetry_verdict},
           "lanes": lanes, "members": members, "diagnostic_selection": picks,
           "reasons": reasons, "links": sorted(set(links))}
    return contracts.CONTRACTS["comparison-report"][2](out)


# Renderings ------------------------------------------------------------------------------

def markdown(doc, base="."):
    lines = ["# Performance release comparison", "",
             "Comparison `%s`: **%s**; evidence verification **%s**. Baseline `%s`, candidate "
             "`%s`, control `%s`." % (doc["comparison_id"], doc["status"], doc["verification"],
                                      doc["sources"]["baseline_sha"],
                                      doc["sources"]["candidate_sha"],
                                      doc["sources"]["control_sha"]), "",
             "Completeness, evidence verification, the performance verdict and calibration "
             "eligibility are separate: a complete comparison may hold a real regression.", "",
             "Core verdict: **%s**; regressions: %s; telemetry: **%s**. Calibration: not "
             "eligible (%s)." % (doc["performance"]["core_verdict"] or "n/a",
                                 ", ".join("`%s`" % r for r in doc["performance"]["regressions"])
                                 or "none", doc["performance"]["telemetry_verdict"] or "n/a",
                                 doc["calibration"]["detail"]), "",
             "| Lane | State | Originals | Coverage | Detail |", "| --- | --- | --- | --- | --- |"]
    for lane in doc["lanes"]:
        c = lane["coverage"]
        lines.append("| %s | %s | %s | %s | %s |" % (
            lane["lane"], lane["state"], ", ".join("`%s`" % m for m in lane["members"]) or "none",
            "%d/%d, %d failed, %d missing" % (c["observed"], c["required"], c["failed"],
                                              c["missing"]) if c else "n/a",
            lane["detail"].replace("|", "/").replace("\n", " ")))
    lines += ["", "| Original | Lane | State | Verification | Measurement |",
              "| --- | --- | --- | --- | --- |"]
    for m in doc["members"]:
        lines.append("| `%s` | %s | %s | %s | %s |" % (m["request_id"], m["lane"], m["state"],
                                                     m["verification"],
                                                     m["measurement_status"] or "n/a"))
    if doc["diagnostic_selection"]:
        lines += ["", "## Diagnostic selection", ""] + [
            "- %s: `%s` %s (%s)" % (p["lane"], p["instance_id"],
                                    "selected" if p["selected"] else "not selected", p["reason"])
            for p in doc["diagnostic_selection"]]
    if doc["reasons"]:
        lines += ["", "## Why not complete", ""] + ["- " + r.replace("\n", " ")
                                                   for r in doc["reasons"]]
    lines += ["", "## Native reports", ""] + ["- [%s](%s)" % (link, os.path.join(base, link))
                                             for link in doc["links"]]
    return ("\n".join(lines) + "\n").encode("utf-8")


def page(doc, base="."):
    e = html.escape
    rows = "".join("<tr><td>%s</td><td>%s</td><td>%s</td><td>%s</td><td>%s</td></tr>" % (
        e(lane["lane"]), e(lane["state"]), e(", ".join(lane["members"])),
        e("%d/%d, %d failed, %d missing" % (lane["coverage"]["observed"],
          lane["coverage"]["required"], lane["coverage"]["failed"], lane["coverage"]["missing"])
          if lane["coverage"] else "n/a"),
        " ".join('<a href="%s">%s</a>' % (e(os.path.join(base, link)), e(link))
                 for link in lane["links"]) or e(lane["detail"])) for lane in doc["lanes"])
    reasons = "".join("<li>%s</li>" % e(r) for r in doc["reasons"])
    return ("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Performance release "
            "comparison</title></head><body><h1>Performance release comparison: %s</h1><p>"
            "Verification %s. Core verdict %s; telemetry %s; calibration not eligible. Baseline "
            "<code>%s</code>, candidate <code>%s</code>. Completeness, verification, the verdict "
            "and calibration eligibility are separate.</p><table><tr><th>Lane</th><th>State</th>"
            "<th>Originals</th><th>Coverage</th><th>Reports</th></tr>%s</table>%s</body></html>\n"
            % (e(doc["status"]), e(doc["verification"]),
               e(doc["performance"]["core_verdict"] or "n/a"),
               e(doc["performance"]["telemetry_verdict"] or "n/a"),
               e(doc["sources"]["baseline_sha"]), e(doc["sources"]["candidate_sha"]), rows,
               "<h2>Why not complete</h2><ul>%s</ul>" % reasons if reasons else "")).encode("utf-8")


def write(doc, output, base):
    """The three renderings into a new directory; links resolve below `base`."""
    os.mkdir(output, 0o755)
    for name, data in zip(REPORT_FILES, (contracts.write_bytes(doc, "comparison-report"),
                                         markdown(doc, base), page(doc, base))):
        with open(os.path.join(output, name), "xb") as stream:
            stream.write(data)


# Portable export -------------------------------------------------------------------------

def _evidence_paths(journal, doc, report_doc):
    """Journal-relative files and directories a self-contained bundle carries."""
    base = os.path.relpath(directory(journal, doc["comparison_id"]), journal.root)
    paths = [os.path.join(base, "comparison.json"), os.path.join(base, "plan.json")]
    agg = os.path.join(base, "aggregate")
    if os.path.isdir(os.path.join(journal.root, agg)):
        # The family's assembled run, its verdict and report with their evidence, whole.
        paths.append(agg)
    for m in report_doc["members"]:
        if m["state"] == "not_submitted":
            continue
        rel = os.path.relpath(journal.path(m["request_id"]), journal.root)
        paths += [os.path.join(rel, name) for name in RECORDS + NATIVE[m["work"]]]
    return [p for p in paths if os.path.lexists(os.path.join(journal.root, p))]


def _links(text, page):
    """The relative link targets of one rendered page (`.html` or `.md`), as bundle paths
    resolved from the page's own directory; external and fragment-only links are skipped."""
    if page.endswith(".html"):
        found = [a or b for a, b in HTML_LINK.findall(text)]
    else:
        found = MD_LINK.findall(text)
    for target in found:
        target = html.unescape(target).split("#", 1)[0].split("?", 1)[0]
        if not target or SCHEME.match(target) or target.startswith("//"):
            continue
        resolved = posixpath.normpath(posixpath.join(posixpath.dirname(page),
                                                     urllib.parse.unquote(target)))
        yield target, resolved


def check_links(bundle, paths):
    """Every relative link of every page among `paths` (bundle-relative files) resolves to a
    file, or a directory holding one, inside the bundle: a copied bundle stays usable offline.
    Raises naming the first page and link that leaves it or names nothing."""
    files = set(paths)
    directories = set()
    for path in files:
        parent = posixpath.dirname(path)
        while parent:
            directories.add(parent)
            parent = posixpath.dirname(parent)
    for page in sorted(path for path in files if path.endswith(PAGES)):
        text = executor.read(os.path.join(bundle, page)).decode("utf-8", "replace")
        for target, resolved in _links(text, page):
            require(not target.startswith("/") and resolved != ".."
                    and not resolved.startswith("../") and (resolved in files
                                                            or resolved in directories),
                    "%s links %s, which the bundle does not hold" % (page, target))


def export(journal, ident, output, installed=executor.ROOT, verify=None):
    """Write a new self-contained bundle at `output`: the verified report and every native
    record it links, below `evidence/` at their journal-relative paths, and `manifest.json`
    hashing every file. Bounded by the executor's `archive_bytes`; the bundle is published by
    one rename, so a refused export leaves nothing."""
    verify = verify or executor.verify_original
    report_doc = status(journal, ident, verify=verify, installed=installed)
    doc, _, _ = load(journal, ident)
    require(not os.path.lexists(output), "%s exists; an export writes a new directory" % output)
    pending = output + ".pending"
    require(not os.path.lexists(pending), "%s exists: an interrupted export; inspect and "
            "remove it" % pending)
    os.mkdir(pending, 0o700)
    try:
        limit = journal.config["archive_bytes"]
        bounds = admission.Bounds(EXPORT_ENTRIES_MAX, 16, limit, limit)
        files, total = [], 0
        for rel in _evidence_paths(journal, doc, report_doc):
            source = os.path.join(journal.root, rel)
            target = os.path.join(pending, EVIDENCE, rel)
            if os.path.isdir(source) and not os.path.islink(source):
                os.makedirs(target, 0o700)
                entries, found = admission.copy_into(source, target, bounds, label=rel)
                for entry in entries:
                    if entry.kind == "file":
                        files.append({"path": "/".join((EVIDENCE, rel, entry.path)),
                                      "size": entry.size, "sha256": found[entry.path]})
                        total += entry.size
            else:
                os.makedirs(os.path.dirname(target), 0o700, exist_ok=True)
                parent, name = os.path.split(source)
                fd, info = admission.open_confined(parent, name, rel)
                try:
                    require(info.st_size <= limit, "%s exceeds the export bound" % rel)
                    digest = admission.copy_fd(fd, info.st_size, target, None, rel)
                finally:
                    os.close(fd)
                files.append({"path": EVIDENCE + "/" + rel, "size": info.st_size,
                              "sha256": digest})
                total += info.st_size
            require(total <= limit and len(files) < EXPORT_ENTRIES_MAX,
                    "the export exceeds the executor's %d-byte archive bound" % limit)
        write(report_doc, os.path.join(pending, "report"), os.path.join("..", EVIDENCE))
        for name in REPORT_FILES:
            data = executor.read(os.path.join(pending, "report", name))
            files.append({"path": "report/" + name, "size": len(data),
                          "sha256": sha256_bytes(data)})
        # A bundle whose pages link anything it does not hold is refused, never published.
        check_links(pending, [item["path"] for item in files])
        raw = executor.read(os.path.join(pending, "report", "report.json"))
        manifest = {"schema": "af.perf-comparison-export/1", "comparison_id": ident,
                    "comparison_digest": report_doc["comparison_digest"],
                    "report_sha256": sha256_bytes(raw), "status": report_doc["status"],
                    "files": sorted(files, key=lambda item: item["path"])}
        with open(os.path.join(pending, MANIFEST), "xb") as stream:
            stream.write(contracts.write_bytes(manifest, "comparison-export"))
        os.rename(pending, output)
    except BaseException:
        admission.remove_tree(pending)
        raise
    return manifest, report_doc


def verify_export(bundle, installed=executor.ROOT):
    """Check a copied bundle offline: exactly the manifest's files with their hashes, a valid
    report whose every link is inside the bundle, every relative link of every bundled page
    (nested native reports included) resolving inside it, and the declaration it names. Then
    the report is derived again (`status` over a read-only view of the bundle, `Bundled`) from
    the bundled declaration, each declared original's state bound to its sealed result, its
    native evidence admitted again (`admit`) and the aggregate's claims recomputed from its
    partitions and run (`aggregate_claims`, which `status` applies to a journal too); a report
    that is not exactly that derivation is refused, so a relabelled report or a rehashed
    manifest proves nothing, and a refused aggregate derives an incomplete, refused report,
    never a complete one. Returns (manifest,
    report). Needs no journal or configuration; `installed` is the trusted measurement
    authority the native telemetry result is verified under."""
    raw = executor.read(os.path.join(bundle, MANIFEST))
    manifest = contracts.read_json(raw, "comparison-export")
    listed = dict((item["path"], item) for item in manifest["files"])
    require(len(listed) == len(manifest["files"]), "the manifest lists a file twice")
    bounds = admission.Bounds(EXPORT_ENTRIES_MAX + 8, 24, contracts.limits.ARCHIVE_BYTES_MAX,
                              contracts.limits.ARCHIVE_BYTES_MAX)
    entries = admission.scan(bundle, bounds, label="comparison export")
    present = set(entry.path for entry in entries if entry.kind == "file")
    require(present == set(listed) | {MANIFEST}, "the bundle's files differ from its manifest: "
            "extra %s, missing %s" % (sorted(present - set(listed) - {MANIFEST})[:8],
                                     sorted(set(listed) - present)[:8]))
    hashes = admission.digests(bundle, [e for e in entries if e.path != MANIFEST],
                               label="comparison export")
    for path, item in listed.items():
        require(hashes[path] == item["sha256"], "%s changed after export" % path)
    report_raw = executor.read(os.path.join(bundle, "report", "report.json"))
    require(sha256_bytes(report_raw) == manifest["report_sha256"], "the report changed")
    report_doc = contracts.read_json(report_raw, "comparison-report")
    require(report_doc["comparison_id"] == manifest["comparison_id"]
            and report_doc["status"] == manifest["status"]
            and report_doc["comparison_digest"] == manifest["comparison_digest"],
            "the manifest describes another report")
    for link in report_doc["links"]:
        require(EVIDENCE + "/" + link in listed, "the report links %s outside the bundle" % link)
    check_links(bundle, listed)
    base = "%s/%s/%s/" % (EVIDENCE, DIRECTORY, manifest["comparison_id"])
    require(base + "comparison.json" in listed and base + "plan.json" in listed,
            "the bundle holds no declaration and frozen plan")
    declaration = contracts.read_json(executor.read(os.path.join(bundle, base, "comparison.json")),
                                      "comparison")
    require(canonical_digest(declaration) == manifest["comparison_digest"],
            "the bundled declaration is not the reported comparison")
    if (base + "aggregate/aggregate.json") in listed:
        agg = contracts.read_json(executor.read(os.path.join(bundle, base, "aggregate",
                                                             "aggregate.json")),
                                  "comparison-aggregate")
        if agg["verdict_sha256"] is not None:
            require(listed.get(base + "aggregate/report/verdict.json", {}).get("sha256")
                    == agg["verdict_sha256"], "the bundled aggregate verdict changed")
    view = Bundled(bundle, declaration, listed)
    verify_native(view)
    # `status` derives the aggregate's claims again (`aggregate_claims`), as in a journal.
    derived = status(view, declaration["comparison_id"], verify=admit(view, installed),
                     installed=installed)
    differ = sorted(key for key in derived if derived[key] != report_doc.get(key))
    require(not differ, "the report's %s are not what its bundled declaration and native "
            "evidence derive (derived %s, %s): a relabelled report proves nothing"
            % (", ".join(differ), derived["status"], "; ".join(derived["reasons"])[:1024]))
    members = set(m["request_id"] for m in derived["members"])
    require(set(view.names()) <= members, "the bundle holds originals %s that are not members "
            "of its derived report" % sorted(set(view.names()) - members)[:8])
    return manifest, report_doc


class Bundled:
    """The read-only journal view `status` derives a report from offline: the bundled
    declaration's journal and the bundled records of its declared originals. A state's
    terminal claim is bound to its sealed result; the frozen configuration is the one a
    bundled original carries under the declaration's digest, never a live one."""

    def __init__(self, bundle, declaration, listed):
        self.bundle, self.listed = bundle, listed
        self.root = os.path.join(bundle, EVIDENCE)
        self.requests = os.path.join(self.root, "requests")
        prefix = EVIDENCE + "/requests/"
        self._names = sorted(set(path[len(prefix):].split("/", 1)[0] for path in listed
                                 if path.startswith(prefix)))
        declared = set([item["request_id"] for item in declaration["partitions"]]
                       + [declaration["telemetry"]["request_id"]]
                       + [lane["request_id"] for lane in declaration["diagnostics"]
                          if lane["request_id"]])
        require(set(self._names) <= declared, "the bundle holds originals %s its declaration "
                "does not name" % sorted(set(self._names) - declared)[:8])
        for ident in self._names:
            contracts.ID.validate(ident, "bundled original")
            for name in ("request.json", "state.json"):
                require(prefix + ident + "/" + name in listed, "bundled original %s has no %s"
                        % (ident, name))
        self.config = {"journal_id": declaration["journal_id"]}
        for ident in self._names:
            if prefix + ident + "/configuration.json" not in listed:
                continue
            config = contracts.read_json(executor.read(os.path.join(
                self.path(ident), "configuration.json")), "executor")
            if canonical_digest(config) == declaration["configuration_digest"]:
                self.config = config
                break
        require("diagnostic" in self.config or "%s/%s/%s/aggregate/aggregate.json" % (
                EVIDENCE, DIRECTORY, declaration["comparison_id"]) not in listed,
                "no bundled original carries the comparison's frozen configuration, so no "
                "aggregate selection or diagnostic lane can be derived")

    def names(self):
        return list(self._names)

    def path(self, ident):
        contracts.ID.validate(ident, "request ID")
        return os.path.join(self.requests, ident)

    def request(self, ident):
        doc = contracts.read_json(executor.read(os.path.join(self.path(ident), "request.json")),
                                  "executor-request")
        # A comparison's members are frozen by its own writer: an earlier request version a
        # journal still reads (ADR-0266) is never a bundled member.
        require(doc["schema"] == contracts.EXECUTOR_REQUEST_SCHEMA, "member %s is an %s "
                "original, not a comparison member" % (ident, doc["schema"]))
        return doc

    def result(self, ident):
        doc = contracts.read_json(executor.read(os.path.join(self.path(ident), "result.json")),
                                  "executor-result")
        require(doc["schema"] == contracts.EXECUTOR_RESULT_SCHEMA, "member %s has an %s "
                "result, not a comparison member's" % (ident, doc["schema"]))
        return doc

    def state(self, ident):
        request = self.request(ident)
        doc = contracts.read_json(executor.read(os.path.join(self.path(ident), "state.json")),
                                  "executor-state")
        require(doc["request_id"] == ident and doc["request_digest"] == canonical_digest(request),
                "state belongs to another original")
        if doc["state"] == "complete" or doc["result_sha256"] is not None:
            # A terminal claim is the sealed result's own status, never an edited label.
            require(doc["result_sha256"] is not None, "original %s is complete without a "
                    "sealed result" % ident)
            result = self.result(ident)
            require(sha256_bytes(contracts.write_bytes(result, "executor-result"))
                    == doc["result_sha256"] and result["status"] == doc["state"]
                    and result["request_digest"] == doc["request_digest"],
                    "original %s's state %s is not its sealed terminal result" % (ident,
                                                                                doc["state"]))
        return doc


# What a complete original of each work must have sealed and bundled for its offline admission.
REQUIRED = {"core": ("plan.json", "run"), "telemetry": ("run",), "profile": ("plan.json",
                                                                          "profile")}


def admit(view, installed=executor.ROOT):
    """The offline admission `status` applies to each complete bundled original (the part of
    `executor.verify_original` its bundled records support): its frozen configuration,
    provenance and sealed result; the native evidence its work needs, sealed and bundled; a
    core run evaluated again to its verdict, a telemetry result verified again under the
    installed authority, a profile's children admitted again. Memoized: the aggregate and the
    report ask once each. This checks the evidence an original sealed; it is never a fresh
    performance acceptance."""
    done = {}

    def verify(journal, ident, installed=installed):
        if ident not in done:
            try:
                done[ident] = (True, _admit(view, ident, installed))
            except (ContractError, OSError) as error:
                done[ident] = (False, error)
        ok, value = done[ident]
        if not ok:
            raise value
        return value
    return verify


def _admit(view, ident, installed):
    request, state = view.request(ident), view.state(ident)
    require(request["backend"] == "local" and state["state"] == "complete",
            "not a completed local original")
    root = view.path(ident)
    rel = "%s/requests/%s" % (EVIDENCE, ident)
    config = contracts.read_json(executor.read(os.path.join(root, "configuration.json")),
                                 "executor")
    require(canonical_digest(config) == request["configuration_digest"]
            and executor.environment(config) == request["environment_digest"],
            "original frozen local configuration changed")
    result = view.result(ident)
    require(result["sources"] == request["sources"] and result["backend"] == "local"
            and result["environment_digest"] == request["environment_digest"]
            and result["producer_digest"] == request["producer_digest"]
            and result["actions"] is None, "local provenance does not match original reservation")
    sealed = set(entry["path"] for entry in result["evidence"])
    what = "member %s" % ident
    needed = REQUIRED[request["work"]] + ((executor.PARENT_VERDICT,) if request["work"]
                                          == "profile" and request["parent"] else ())
    for prefix in needed:
        require(any(entry == prefix or entry.startswith(prefix + "/") for entry in sealed),
                "%s is complete without sealed %s evidence" % (what, prefix))
    for prefix in NATIVE[request["work"]]:
        if any(entry == prefix or entry.startswith(prefix + "/") for entry in sealed):
            require(any(path == rel + "/" + prefix or path.startswith(rel + "/" + prefix + "/")
                        for path in view.listed), "%s's sealed %s is not bundled"
                    % (what, prefix))
    if request["work"] == "core":
        plan = executor.read(os.path.join(root, "plan.json"))
        require(sha256_bytes(plan) == request["plan_sha256"], "%s: original plan changed" % what)
        _same_verdict(root, what)
    elif request["work"] == "telemetry":
        measured, _, _ = telemetrylab.verify(os.path.join(root, "run"), installed)
        require(measured["result_id"] == "local_" + ident[4:],
                "%s: the bundled telemetry result is another original's" % what)
    else:
        raw = executor.read(os.path.join(root, "plan.json"))
        require(sha256_bytes(raw) == request["plan_sha256"], "%s: diagnostic parent plan "
                "changed" % what)
        if request["parent"] is not None:
            require(sha256_bytes(executor.read(os.path.join(root, executor.PARENT_VERDICT)))
                    == request["parent"]["verdict_sha256"], "%s: the frozen parent verdict "
                    "changed" % what)
        report.admit_children([os.path.join(root, "profile")], contracts.read_json(raw, "run"))
    return request, state, result


def aggregate_claims(view, doc, agg, admitted):
    """The aggregate's claims derived again, in a journal or a copied bundle (`Bundled`): it
    names exactly the declaration's comparison and frozen plan; each partition entry is its
    original's terminal state and sealed result, included and `verified` exactly when that
    original is this comparison's complete member, among the `admitted` originals and, for a
    later partition, carries its declared execution window (`_window`), as `aggregate`
    writes it; otherwise `refused` when complete and `not_complete` when not, never included;
    the family's run holds exactly the included partitions' shards, byte for byte, on the
    first partition's build, and evaluated again reproduces its verdict (`_same_verdict`);
    and its result, coverage and regressions are that verdict's. An aggregate that judged
    nothing claims nothing. Raises naming the first claim that is not derived."""
    if agg is None:
        return
    where = "the aggregate"
    require(agg["comparison_id"] == doc["comparison_id"]
            and agg["plan_sha256"] == doc["plan_sha256"],
            "%s names comparison %s and plan %s, not its declaration's %s and %s"
            % (where, agg["comparison_id"], agg["plan_sha256"], doc["comparison_id"],
               doc["plan_sha256"]))
    require([p["index"] for p in agg["partitions"]] == [p["index"] for p in doc["partitions"]],
            "%s does not state every declared partition" % where)
    included = []
    for item, entry in zip(doc["partitions"], agg["partitions"]):
        state = _state(view, item["request_id"])
        require(state is not None and state["state"] in executor.TERMINAL
                and entry["request_id"] == item["request_id"]
                and entry["shards"] == item["shards"] and entry["state"] == state["state"]
                and entry["result_sha256"] == state["result_sha256"],
                "%s's partition %d is not its original" % (where, item["index"]))
        why = _binding(view, doc, view.request(item["request_id"]), "core", item["key"],
                       _partition(doc, item))
        complete = state["state"] == "complete"
        if why is not None:
            why = "which is not this comparison's member: %s" % why
        elif not complete:
            why = "whose original is %s" % state["state"]
        elif item["request_id"] not in admitted:
            why = "whose original %s is not admitted" % item["request_id"]
        else:
            try:
                _window(view, doc, item)
            except (ContractError, OSError) as error:
                why = "whose execution window is refused: %s" % str(error)[:512]
        require(not entry["included"] or why is None, "%s includes partition %d, %s"
                % (where, item["index"], why))
        require(entry["included"] or why is not None, "%s excludes partition %d, whose "
                "original %s is this comparison's admitted complete member" % (
                    where, item["index"], item["request_id"]))
        verification = ("verified" if why is None else "refused" if complete
                        else "not_complete")
        require(entry["verification"] == verification, "%s states partition %d %s, not its "
                "derived %s" % (where, item["index"], entry["verification"], verification))
        if why is None:
            included.append(item)
    if agg["verdict_sha256"] is None:
        require(not agg["partitions"][0]["included"], "%s judged nothing over a verified "
                "first partition" % where)
        require(agg["builds_sha256"] is None and agg["result"] is None
                and agg["coverage"] is None and not agg["regressions"],
                "%s judged nothing, yet claims a build, result, coverage or regressions" % where)
        return
    require(agg["partitions"][0]["included"], "%s judged a family without its first "
            "partition's verified build" % where)
    path = os.path.join(directory(view, doc["comparison_id"]), "aggregate")
    first = executor.read(os.path.join(view.path(doc["partitions"][0]["request_id"]), "run",
                                       "builds.json"))
    require(sha256_bytes(first) == agg["builds_sha256"] == sha256_bytes(executor.read(
            os.path.join(path, "run", "builds.json"))), "%s is not on the first partition's "
            "build" % where)
    held = {}
    for item in included:
        own = os.path.join(view.path(item["request_id"]), "run", "shards")
        for index in item["shards"]:
            for name, digest in _files(view, os.path.join(own, str(index))).items():
                held["%d/%s" % (index, name)] = digest
    require(_files(view, os.path.join(path, "run", "shards")) == held,
            "%s's run is not exactly its included partitions' shards" % where)
    _same_verdict(path, "the family aggregate")
    coverage, performance = overview._core(path)
    require(agg["result"] == performance["verdict"] and agg["coverage"] == coverage
            and agg["regressions"] == performance["regressions"],
            "%s's result, coverage or regressions are not its verdict's" % where)


def _files(view, path):
    """{path below `path`: sha256} of every file below the directory `path`: in a copied bundle
    its manifest's (each already checked against its bytes), in a journal each file's own,
    enumerated and read within the export's bounds."""
    if isinstance(view, Bundled):
        prefix = os.path.relpath(path, view.bundle).replace(os.sep, "/") + "/"
        return dict((name[len(prefix):], item["sha256"]) for name, item in view.listed.items()
                    if name.startswith(prefix))
    if not os.path.isdir(path) or os.path.islink(path):
        return {}
    bounds = admission.Bounds(EXPORT_ENTRIES_MAX, 16, contracts.limits.ARCHIVE_BYTES_MAX,
                              contracts.limits.ARCHIVE_BYTES_MAX)
    label = os.path.relpath(path, view.root)
    entries = admission.scan(path, bounds, label=label)
    return admission.digests(path, entries, label=label)


def _same_verdict(root, what):
    """The native core run bundled at `root/run`, evaluated again offline, reproduces exactly
    the verdict bundled at `root/report/verdict.json`: its coverage and results are verified,
    not taken from the export's own hashes."""
    evaluation = compare.evaluate(os.path.join(root, "run"))
    bundled = contracts.read_json(executor.read(os.path.join(root, "report",
                                                             report.VERDICT_JSON)), "verdict")
    require(evaluation.verdict == bundled, "%s: its bundled run does not reproduce its bundled "
            "verdict" % what)


def verify_native(view):
    """Every bundled file of each bundled original with a sealed terminal result, outside its
    journal records, is a file that result names, with that hash. An original without one is
    never admitted, so nothing of it is read as evidence."""
    for ident in view.names():
        rel = "%s/requests/%s" % (EVIDENCE, ident)
        if view.state(ident)["result_sha256"] is None:
            continue
        sealed = dict((entry["path"], entry["sha256"]) for entry in view.result(ident)["evidence"])
        for path, item in view.listed.items():
            if not path.startswith(rel + "/") or path[len(rel) + 1:] in RECORDS:
                continue
            require(sealed.get(path[len(rel) + 1:]) == item["sha256"], "member %s's bundled "
                    "%s is not evidence its sealed result names" % (ident, path))


def run(journal_factory, ident, cycle, installed=executor.ROOT, rounds=None):
    """Drive a launched comparison to its end: advance, then one `cycle` per queued original,
    until nothing is queued and advancing submits nothing new. Each step reopens the journal,
    so `status` reads it in between; the result is the durable status, never this loop's
    exit. `rounds` bounds the loop (default: two per declared original). A cancellation of
    the controller (`runner.Cancelled`, a signal a shard consumed) leaves the loop as it
    leaves the cycle that settled it: no later original of the comparison is claimed."""
    with journal_factory(True) as journal:
        doc, _, _ = load(journal, ident)
    limit = rounds or 2 * (len(doc["partitions"]) + 3) + 2
    outcomes = []
    for _ in range(limit):
        with journal_factory(False) as journal:
            advance(journal, ident, installed)
            outcome = cycle(journal)
        outcomes.append(outcome)
        if outcome == "busy":
            break
        if outcome == "idle":
            with journal_factory(False) as journal:
                before = len(journal.names())
                advance(journal, ident, installed)
                if len(journal.names()) == before:
                    break
    with journal_factory(True) as journal:
        return status(journal, ident, installed=installed), outcomes
