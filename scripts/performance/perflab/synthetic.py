"""Synthetic evidence for tests and committed fixtures; always labeled synthetic.

Every bundle written here carries `provenance = "synthetic"` in each shard, a host whose
virtualization is `synthetic`, and binaries hashed from fixed labels. A synthetic verdict can
never satisfy the required check, and none of these numbers describe the AF product.
"""
from fractions import Fraction
import hashlib
import json
import os

from . import bundle, contracts, images, planning, schedule
from .values import document_bytes, format_decimal, sha256_bytes

EMPTY = sha256_bytes(b"")
BASE_TIME_MS = 1760000000000
# A synthetic run's execution starts a minute before its first shard.
RUN_STARTED_MS = BASE_TIME_MS - 60000


def label_sha(label):
    return hashlib.sha1(("af.perf-synthetic/1\x00" + label).encode("utf-8")).hexdigest()


def label_digest(label):
    return sha256_bytes(("af.perf-synthetic/1\x00" + label).encode("utf-8"))


SYNTHETIC_HOST = dict((key, {"unknown": "synthetic: no host was observed"})
                      for key in contracts.HOST_KEYS)
SYNTHETIC_HOST["virtualization"] = {"value": "synthetic"}
SYNTHETIC_LOAD = dict((key, {"unknown": "synthetic: nothing was observed"})
                      for key in contracts.LOAD_KEYS)


def identities(trigger="manual", baseline="baseline", candidate="candidate"):
    ids = planning.Identities()
    ids.baseline = label_sha(baseline)
    ids.candidate = label_sha(candidate)
    ids.control = ids.baseline
    if trigger == "manual":
        ids.trigger = {"kind": "manual", "remeasure_of": None}
    else:
        raise ValueError("synthetic identities support the manual trigger")
    return ids


def plan(suite, scope="bootstrap", tier="quick", run_id="synthetic_run", seed=7, cases=None,
         shards=None, pairs=None, mode=None, baseline="baseline", candidate="candidate",
         build_image=None, fixtures="fixtures"):
    """Freeze a plan over synthetic identities, without Git; `fixtures` labels the declared
    fixture definitions' digest."""
    recipe = suite.doc["recipes"][suite.doc["tiers"][tier]["profiles"][0]["recipe"]]
    # Nothing is built: an unpinned Linux recipe is frozen as an explicitly trusted host build
    # unless a test freezes a configured image.
    trusted_host = recipe["os"] == "linux" and "container_image" not in recipe \
        and build_image is None
    request = planning.Request(trigger="manual", tier=tier, scope=scope, base=baseline,
                               head=candidate, run_id=run_id, seed=seed, cases=cases or [],
                               shards=shards, pairs=pairs, trusted_host_build=trusted_host,
                               build_image=build_image)
    ids = identities(baseline=baseline, candidate=candidate)
    authority = ids.control
    activation = None
    mode = mode or ("active" if scope == "full" else "control")
    if mode == "active":
        activation = {"sha256": label_digest("activation"), "admission": "calibrated",
                      "readiness_sha256": label_digest("readiness"),
                      "cohort_id": "synthetic_cohort",
                      "calibrated_groups": [{"group_id": "synthetic_group",
                                             "cpu_model": SYNTHETIC_CPU,
                                             "image": label_digest("image")}]}
    digests = {"suite_sha256": suite.raw_sha256, "policy_digest": suite.policy_digest,
               "instrument_digest": label_digest("instrument"),
               "fixture_digest": label_digest(fixtures)}
    # A synthetic source's Cargo.lock digest is derived from its label, as its SHA is.
    inputs = {"authority": {"cargo_lock_sha256": label_digest(baseline + "/lock")},
              "baseline": {"cargo_lock_sha256": label_digest(baseline + "/lock")},
              "candidate": {"cargo_lock_sha256": label_digest(candidate + "/lock")}}
    return planning.freeze(suite, request, ids, mode, authority, activation, digests, [],
                           inputs)


def binaries(side):
    return {"af": label_digest(side + "/af"), "probe": label_digest(side + "/probe")}


def _step(argv):
    return {"status": "succeeded", "argv": argv, "environment": {}, "deadline_seconds": 1200,
            "elapsed_ms": 0, "exit_code": 0,
            "log": {"sha256": EMPTY, "bytes": 0, "truncated_bytes": 0}}


def _artifact(role, name, digest):
    return {"path": "artifacts/%s/%s" % (role, name), "sha256": digest, "size": 0}


def run_execution(run_plan):
    total = run_plan["limits"]["run_total_minutes"]
    return {"started_unix_ms": RUN_STARTED_MS,
            "deadline_unix_ms": RUN_STARTED_MS + total * 60000, "total_minutes": total}


def _isolation(run_plan):
    frozen = run_plan["profile"]["build"]
    isolation = {"kind": "host_process", "environment": "allowlist",
                 "credential_free_git": True, "cargo_offline": True, "image": None,
                 "compile_network": "host", "dependency_fetch": "prefetched",
                 "filesystem": "trusted_host", "detail": "synthetic: nothing was built"}
    if frozen["isolation"] == "container":
        reference = frozen["image"]
        isolation.update(kind="container", compile_network="none",
                         filesystem="run_mounts_only",
                         image={"reference": reference,
                                "id": (reference if images.is_image_id(reference)
                                       else label_digest("image-id")),
                                "platform": "linux/amd64"})
    return isolation


def builds(run_plan):
    """Build evidence that honestly follows the plan: its recipe, frozen build, bound inputs
    and run deadline."""
    settings = contracts.recipe_settings(run_plan["profile"]["recipe"])
    inputs = run_plan["build_inputs"]
    sides = {}
    for side, key, role in ((schedule.BASELINE, "baseline_sha", "base"),
                            (schedule.CANDIDATE, "candidate_sha", "head")):
        sha = run_plan["resolved"][key]
        product = _step(["synthetic-build"])
        sides[side] = {
            "source_sha": sha, "status": "succeeded", "worktree": role, "shared_with": None,
            "adapter_outcomes": [{"adapter_id": "synthetic_adapter", "priority": 1,
                                  "equivalence_group": "synthetic_group",
                                  "outcome": "compiled", "step": _step(["synthetic-probe"])}],
            "chosen_adapter": "synthetic_adapter", "equivalence_group": "synthetic_group",
            "probe_coverage": "available", "coverage_reason": None,
            "overlay": {"adapter_id": "synthetic_adapter", "digest": label_digest("overlay"),
                        "files": [{"path": "crates/synthetic/examples/probe.rs",
                                   "sha256": label_digest("overlay/probe.rs")}]},
            "binaries": binaries(side),
            "artifacts": {"af": _artifact(side, "af", binaries(side)["af"]),
                          "probe": _artifact(side, "perf-probe", binaries(side)["probe"])},
            "embedded_commit": {"expected": sha[:12], "verified": True,
                                "method": "binary_bytes"},
            "source_unchanged": True,
            "cargo_lock_sha256": inputs[side]["cargo_lock_sha256"],
            "settings": dict(settings), "argv": ["synthetic-build"], "environment": {},
            "product": product, "log": dict(product["log"]),
        }
    supervisor = label_digest("supervisor")
    return {"schema": contracts.BUILDS_SCHEMA, "run_id": run_plan["run_id"],
            "plan_digest": run_plan["plan_digest"],
            "recipe_id": run_plan["profile"]["recipe_id"],
            "recipe_digest": run_plan["profile"]["recipe_digest"],
            "provenance": "synthetic",
            "execution": {"run": run_execution(run_plan), "caller_deadline_unix_ms": None,
                          "deadline_unix_ms": run_execution(run_plan)["deadline_unix_ms"],
                          "ended_unix_ms": BASE_TIME_MS - 1000},
            "build_cache": {"treatment": "cold", "scope": "run_worktree_targets",
                            "identity": label_digest("build-cache"), "priming": None,
                            "observed": [{"worktree": "authority", "state": "empty",
                                          "compiled_bytes": 0}],
                            "measured_started_unix_ms": run_execution(run_plan)[
                                "started_unix_ms"]},
            "isolation": _isolation(run_plan),
            "toolchain": {"rustc": "synthetic", "cargo": "synthetic", "host": "synthetic"},
            "linker": {"kind": settings["linker"], "identity": None,
                       "unavailable": "synthetic: nothing was linked"},
            "supervisor": {"source_sha": run_plan["resolved"]["authority_sha"],
                           "status": "succeeded", "binary_sha256": supervisor,
                           "artifact": _artifact("supervisor", "perf-supervisor", supervisor),
                           "source_unchanged": True,
                           "cargo_lock_sha256": inputs["authority"]["cargo_lock_sha256"],
                           "settings": dict(settings), "step": _step(["synthetic-supervisor"]),
                           "log": {"sha256": EMPTY, "bytes": 0, "truncated_bytes": 0}},
            "sides": sides,
            "adapter_binding": {"baseline": "synthetic_adapter",
                                "candidate": "synthetic_adapter",
                                "baseline_group": "synthetic_group",
                                "candidate_group": "synthetic_group", "equivalent": True},
            "artifacts": {"manifest_sha256": label_digest("artifacts"), "bytes": 0,
                          "limit_bytes": run_plan["limits"]["build_artifact_bytes"]},
            "budgets": {"worktree_limit_bytes": run_plan["limits"]["worktree_bytes"],
                        "worktree_peak_bytes": 0, "build_log_limit_bytes": 4194304},
            "cleanup": {"status": "removed", "detail": "synthetic: nothing was created"}}


def _shard_started(shard):
    """When synthetic shard `shard` starts: shards run one after another, within the run."""
    return BASE_TIME_MS + shard * 1000000


def constant(ms):
    """A value function: every execution takes `ms` (a Fraction or decimal string)."""
    value = Fraction(ms)
    return lambda slot, case: value


def _containment(run_plan, provenance):
    """(mode, containment, image) a synthetic shard reports: `synthetic`, unless a test
    relabels it measured, when it is the plan environment's own."""
    linux = run_plan["profile"]["environment"]["containment"] == "linux_container"
    if provenance == "synthetic":
        return "synthetic", "process_group", None
    if linux:
        return "linux_container", "pid_namespace", label_digest("image")
    return "trusted_local", "process_group", None


def record(run_plan, shard, slot, value, failure=None, containment="process_group"):
    """One synthetic sample for `slot`; `failure` is a failure kind or None."""
    cases = dict((case["instance_id"], case) for case in run_plan["cases"])
    workload = run_plan["health"]["workload"]
    if slot["instance_id"] == workload:
        kind, primary, optional = "library", run_plan["health"]["metric"], []
    else:
        case = cases[slot["instance_id"]]
        kind = case["kind"]
        primary = case["primary_metric"]["key"]
        optional = [metric["key"] for metric in case["optional_metrics"]]
    side = slot["side"]
    source = run_plan["resolved"]["baseline_sha" if side == schedule.BASELINE
                                  else "candidate_sha"]
    metrics = {}
    scopes = {}
    if failure is None:
        metrics[primary] = {"value": format_decimal(value), "unit": "ms"}
        sentinel = slot["instance_id"] == workload
        boundary = "library_region" if sentinel else case["timing"]["boundary"]
        scopes[primary] = "probe_reported" if boundary == "reported_interval" else boundary
    oracle_kind = (run_plan["health"]["oracle"]["kind"] if slot["instance_id"] == workload
                   else case["oracle"]["kind"])
    return {
        "schema": contracts.SAMPLE_SCHEMA, "run_id": run_plan["run_id"],
        "plan_digest": run_plan["plan_digest"], "shard": shard, "sequence": slot["sequence"],
        "phase": slot["phase"], "instance_id": slot["instance_id"], "pair": slot["pair"],
        "position": slot["position"], "side": side, "source_sha": source,
        "binary_sha256": binaries(side)["af" if kind == "process" else "probe"],
        "host_id": contracts.host_id(SYNTHETIC_HOST),
        "started_unix_ms": _shard_started(shard) + slot["sequence"],
        "status": "ok" if failure is None else "failed",
        "identity": {"case_id": (workload if slot["instance_id"] == workload
                                 else case["case_id"]),
                     "contract_id": None, "adapter": None,
                     "parameters": ({} if slot["instance_id"] == workload
                                    else dict(case["parameters"])),
                     "fixture_digest": None},
        "metrics": metrics, "metric_scopes": scopes,
        "unavailable_metrics": dict((key, "synthetic_fixture") for key in optional),
        "oracle": {"status": ("passed" if failure is None else
                              "failed" if failure == "oracle_mismatch" else "not_run"),
                   "kind": oracle_kind, "observed_digest": None, "detail": "synthetic"},
        "execution": {"containment": containment,
                      "termination": "exited" if failure is None else None,
                      "exit_code": 0 if failure is None else None, "signal": None,
                      "exit_observation": "unreaped" if failure is None else None,
                      "lifecycle": None, "host_wall_ns": None, "fixture_setup_ns": None},
        "failure": None if failure is None else {"kind": failure, "detail": "synthetic"},
        "logs": {"stdout_bytes": 0, "stderr_bytes": 0, "stdout_sha256": EMPTY,
                 "stderr_sha256": EMPTY, "truncated_bytes": 0, "retained": None},
    }


def write_run(run_plan, run_dir, value, sentinel=None, failures=None, omit=(),
              shard_status=None, edit=None, edit_builds=None, provenance="synthetic"):
    """Write a complete synthetic run directory with consistent manifests.

    `value(slot, case)` gives each execution's primary metric in ms; `sentinel(slot)` the
    sentinel times (default 10 ms). `failures` maps slot keys to failure kinds; `omit` lists
    slot keys left unrecorded; `shard_status` maps shard index to a terminal status;
    `edit(record)` returns the records written in a record's place (tests use it to write
    well-formed but wrong evidence); `edit_builds(doc)` changes the build manifest.
    `provenance` exists only so tests can exercise the measured-evidence rule.
    """
    failures = failures or {}
    shard_status = shard_status or {}
    os.makedirs(run_dir)
    _write(os.path.join(run_dir, "run.json"), document_bytes(run_plan))
    builds_doc = builds(run_plan)
    if edit_builds is not None:
        edit_builds(builds_doc)
    builds_bytes = contracts.write_bytes(builds_doc, "builds")
    _write(os.path.join(run_dir, "builds.json"), builds_bytes)
    bundle.write_manifest(run_dir, 1 << 30, skip=(SHARD_DIR,))
    cases = dict((case["instance_id"], case) for case in run_plan["cases"])
    for shard in run_plan["shards"]:
        index = shard["index"]
        root = os.path.join(run_dir, SHARD_DIR, str(index))
        os.makedirs(root)
        records = []
        slots = schedule.expected_slots(run_plan, index)
        for slot in slots:
            key = schedule.slot_key(slot)
            if key in omit:
                continue
            if slot["phase"].startswith("sentinel"):
                amount = sentinel(slot) if sentinel else Fraction(10)
            else:
                amount = value(slot, cases[slot["instance_id"]])
            made = record(run_plan, index, slot, amount, failures.get(key),
                          _containment(run_plan, provenance)[1])
            records.extend(edit(made) if edit is not None else [made])
        data = b"".join(document_bytes(item) for item in records)
        status = shard_status.get(index, "complete")
        shard_doc = {
            "schema": contracts.SHARD_SCHEMA, "run_id": run_plan["run_id"],
            "plan_digest": run_plan["plan_digest"], "shard": index, "status": status,
            "provenance": provenance, "host_id": contracts.host_id(SYNTHETIC_HOST),
            "host": SYNTHETIC_HOST,
            "execution": {"mode": _containment(run_plan, provenance)[0],
                          "containment": _containment(run_plan, provenance)[1],
                          "sample_image": _containment(run_plan, provenance)[2],
                          "supervisor_sha256": label_digest("supervisor"),
                          "builds_sha256": sha256_bytes(builds_bytes),
                          "sampling_deadline_ms": run_plan["limits"]["shard_sampling_minutes"]
                          * 60000,
                          "run": builds_doc["execution"]["run"],
                          "caller_deadline_unix_ms": None,
                          "deadline_unix_ms": min(
                              builds_doc["execution"]["run"]["deadline_unix_ms"],
                              _shard_started(index)
                              + run_plan["limits"]["shard_sampling_minutes"] * 60000),
                          "sample_cpus": None,
                          "coordination_lock": (None if provenance == "synthetic" else
                                                "/tmp/af-perf-machine.lock")},
            "seeds": [],
            "observations": {"start": SYNTHETIC_LOAD, "end": SYNTHETIC_LOAD},
            "expected_records": len(slots),
            "record_count": len(records),
            "slots": {"failed": sum(1 for item in records if item["status"] == "failed"),
                      "unstarted": max(0, len(slots) - len(records))},
            "samples_sha256": sha256_bytes(data), "logs_bytes": 0,
            "samples_bytes": len(data), "started_unix_ms": _shard_started(index),
            "ended_unix_ms": _shard_started(index) + len(slots),
            "failure": None if status == "complete" else {"reason": "synthetic_interruption",
                                                          "detail": "synthetic"},
        }
        _write(os.path.join(root, "samples.jsonl"), data)
        _write(os.path.join(root, "shard.json"), contracts.write_bytes(shard_doc, "shard"))
        bundle.write_manifest(root, 1 << 30)
    return run_dir


SHARD_DIR = "shards"


SYNTHETIC_CPU = "Synthetic CPU"


def observe(run_dir, run_plan, cpu_model=SYNTHETIC_CPU, image=None):
    """Rewrite every shard of a written run as observed on a host of `cpu_model` and sample
    `image` (by default the synthetic activation's calibrated group): its records' host IDs and
    its shard.json. Tests use it to place measured-labelled evidence inside or outside a
    calibrated CPU/image group (ADR-0184); nothing here observed a host."""
    observed = dict(SYNTHETIC_HOST, cpu_model={"value": cpu_model} if cpu_model
                    else {"unknown": "synthetic: no CPU model was observed"})
    for shard in run_plan["shards"]:
        root = os.path.join(run_dir, SHARD_DIR, str(shard["index"]))
        path = os.path.join(root, "samples.jsonl")
        with open(path, "rb") as stream:
            records = [json.loads(line) for line in stream.read().splitlines() if line]
        for record in records:
            record["host_id"] = contracts.host_id(observed)
        os.chmod(path, 0o644)
        with open(path, "wb") as stream:
            stream.write(b"".join(document_bytes(record) for record in records))

        def edit(doc):
            doc["host"] = observed
            doc["host_id"] = contracts.host_id(observed)
            doc["execution"]["sample_image"] = image or label_digest("image")
        reseal_shard(root, edit)
    return run_dir


def reseal_shard(root, edit_doc=None):
    """Rewrite a shard's shard.json and manifest over its current samples.jsonl.

    Tests change samples.jsonl (or, through `edit_doc(doc)`, the terminal record) and reseal it
    so that every hash is fresh and valid: the comparator must then refuse the content itself.
    """
    with open(os.path.join(root, "samples.jsonl"), "rb") as stream:
        data = stream.read()
    doc = contracts.read_file(os.path.join(root, "shard.json"), "shard")
    count = data.count(b"\n")
    doc.update(record_count=count, samples_sha256=sha256_bytes(data), samples_bytes=len(data))
    doc["slots"]["unstarted"] = max(0, doc["expected_records"] - count)
    if edit_doc is not None:
        edit_doc(doc)
    _write(os.path.join(root, "shard.json"), contracts.write_bytes(doc, "shard"))
    os.remove(os.path.join(root, bundle.MANIFEST))
    bundle.write_manifest(root, 1 << 30)
    return root


def _write(path, data):
    with open(path, "wb") as stream:
        stream.write(data)
