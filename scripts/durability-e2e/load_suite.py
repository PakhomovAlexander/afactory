"""Run and compare a bounded native durability cohort on existing AF broker workers."""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import json
import math
import os
from pathlib import Path
import re
import secrets
import subprocess
import threading
import time

import compare
from control import Control, HTTPError, save
from load_cases import CASES, DIGEST_PATTERN, FIXTURES, MARKER, SUITE_FILES, digest, files_identity, load_profiles

SUITE = Path(__file__).resolve().parent
CHECKOUT = SUITE.parents[1]
BINDINGS_SCHEMA = "af-durability/load-workers@1"
RUN_SCHEMA = "af-durability/load-report@1"
WORKER_PATH = r"/[A-Za-z0-9_./-]+"
STATUS = {"pass": "pass", "fail": "engine-failure", "invalid": "invalid/harness",
          "unsupported": "unsupported"}
CLOCK = "worker monotonic case subprocess"
METRICS = ("terminal_s", "cpu_s", "simulated_tokens", "provider_tokens", "unknown_settlements")


def load_workers(path):
    manifest = json.loads(path.read_text())
    if set(manifest) != {"schema", "workers"} or manifest["schema"] != BINDINGS_SCHEMA:
        raise ValueError(f"expected {BINDINGS_SCHEMA}")
    workers = manifest["workers"]
    if not isinstance(workers, list) or not 2 <= len(workers) <= 16:
        raise ValueError("bind between two and sixteen allocated workers")
    bindings = {}
    for row in workers:
        paths = ("binary", "source", "suite", "scratch")
        digests = ("binary_sha256", "source_workload")
        if set(row) != {"worker", "source_commit", *paths, *digests}:
            raise ValueError("each worker requires an explicit path and pin for binary, source, suite and scratch")
        worker = row["worker"]
        if not re.fullmatch(r"r-[1-9][0-9]{0,7}", worker) or worker in bindings:
            raise ValueError(f"invalid or duplicate allocation handle: {worker!r}")
        for name in paths:
            path = Path(row[name])
            if (not re.fullmatch(WORKER_PATH, row[name]) or str(path) != row[name]
                    or ".." in path.parts or row[name].startswith("//") or len(row[name]) > 300):
                raise ValueError(f"{worker}: use a canonical absolute {name} path")
        if (not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", row["source_commit"])
                or any(not re.fullmatch(DIGEST_PATTERN, row[name]) for name in digests)):
            raise ValueError(f"{worker}: invalid pin")
        bindings[worker] = {name: row[name] for name in ("source_commit", *paths, *digests)}
    if len({row["source_workload"] for row in bindings.values()}) != 1:
        raise ValueError("all workers must run the same pinned source fixture bytes")
    if len({row["binary_sha256"] for row in bindings.values()}) != 1:
        raise ValueError("all workers must run the same pinned AF build")
    return bindings


def schedule(run, cases, workers, repeat, seed):
    rows = []
    for replicate in range(repeat):
        offset = replicate * (len(cases) % len(workers) or 1)
        for index, case in enumerate(cases):
            worker = workers[(index + offset + seed) % len(workers)]
            rows.append({"job": f"{run}-{len(rows):04}", "worker": worker, "case": case,
                         "replicate": replicate + 1, "seed": seed + replicate})
    return rows


def worker_args(binding, run_id, job, phase, seconds, *arguments):
    return ["/usr/bin/timeout", "--kill-after=5s", f"{seconds}s",
            "/usr/bin/env", "-i", "PATH=/usr/local/bin:/usr/bin:/bin", "LANG=C.UTF-8",
            "/usr/bin/python3", "-B", "-X", "pycache_prefix=/dev/null",
            binding["suite"] + "/load_worker.py", phase,
            json.dumps(binding, separators=(",", ":")), run_id, job, *arguments]


def records(path, job):
    if not path.is_file():
        return {}
    values = {}
    for line in json.loads(path.read_text())["output"].splitlines():
        _, found, payload = line.partition(MARKER)
        if not found:
            continue
        item = json.loads(payload)
        if item["job"] != job or item["event"] in values:
            raise ValueError(f"unexpected or duplicate worker log record for {job}")
        values[item["event"]] = item
    return values


def validated_trial(event, row):
    verdict = event["verdict"]
    metrics = event["metrics"]
    if verdict not in STATUS or not event.get("reason") or not isinstance(metrics, dict):
        raise ValueError("worker result has an invalid verdict, reason or metric map")
    if metrics.keys() - compare.METRICS or any(
            type(value) not in {int, float} or not math.isfinite(value) or value < 0
            for value in metrics.values()):
        raise ValueError("worker result has an invalid measurement")
    if verdict in {"pass", "fail"} and CASES[row["case"]].expectation != "control":
        if not event.get("fault_witness"):
            raise ValueError("worker reported a decisive result without a fault receipt")
    if verdict == "pass" and event.get("violations"):
        raise ValueError("worker reported a pass with invariant violations")
    interval = event.get("interval_ns")
    if interval is not None and (not isinstance(interval, list) or len(interval) != 2
                                 or any(type(n) is not int for n in interval)
                                 or not 0 <= interval[0] < interval[1]):
        raise ValueError("worker result has an invalid monotonic case interval")
    return event


def check_jobs(control, worker, path, own=()):
    jobs = control.request(f"/v1/resources/{worker}/jobs")
    save(path, jobs)
    for job in jobs:
        terminal = job["state"] in {"inactive", "failed"} or job["substate"] == "exited"
        if not terminal and job.get("name") not in own:
            raise ValueError(f"{worker}: active unrelated broker job {job.get('name', '<unknown>')}")
    return jobs


def bounded_reason(error):
    return (f"{type(error).__name__}: {error}")[-3500:]


def collect_logs(control, worker, job, directory):
    if not directory.is_dir() or (directory / "logs.json").exists():
        return
    try:
        save(directory / "logs.json", control.request(f"/v1/resources/{worker}/jobs/{job}/logs"))
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        save(directory / "logs-error.json", {"reason": bounded_reason(error)})


def preflight(control, directory, run_id, bindings, rows, slots):
    resources = control.request("/v1/resources")
    save(directory / "resources.json", resources)
    by_id = {row["id"]: row for row in resources}
    counts = Counter(row["worker"] for row in rows)
    # Check every allocation before submitting even the read-only preflight probes.
    for worker in bindings:
        available = by_id.get(worker, {})
        if (available.get("profile"), available.get("state"), available.get("power")) != (
                "af", "allocated", "running"):
            raise ValueError(f"{worker}: requires an allocated, running AF profile")
        check_jobs(control, worker, directory / f"{worker}-initial-jobs.json")

    def probe(worker, binding):
        job = run_id + "-probe"
        output = directory / worker / "preflight"
        options = ["--slots", str(slots), "--trials", str(counts[worker])]
        if any(CASES[row["case"]].namespace for row in rows if row["worker"] == worker):
            options.append("--namespace")
        try:
            ok = control.job(worker, job, worker_args(binding, run_id, job, "preflight", 60, *options),
                             output, 120)
            events = records(output / "logs.json", job)
            if not ok or "preflight" not in events:
                raise ValueError(events.get("error", {}).get("reason", "preflight produced no valid receipt"))
            receipt = events["preflight"]
            if any(receipt["pins"][key] != binding[key] for key in receipt["pins"]):
                raise ValueError("preflight returned different workload pins")
            return receipt
        except (OSError, RuntimeError, ValueError, KeyError, subprocess.SubprocessError) as error:
            if not (isinstance(error, HTTPError) and error.status == 409):
                collect_logs(control, worker, job, output)
            return {"error": bounded_reason(error)}

    with ThreadPoolExecutor(max_workers=len(bindings)) as pool:
        futures = {worker: pool.submit(probe, worker, binding) for worker, binding in bindings.items()}
        probes = {worker: future.result() for worker, future in futures.items()}
    save(directory / "preflight.json", probes)
    blocked = [f"{worker}: {receipt['error']}" for worker, receipt in probes.items() if "error" in receipt]
    if blocked:
        raise ValueError("preflight refused the cohort. " + " | ".join(blocked))
    if len({receipt["boot_id"] for receipt in probes.values() if receipt["boot_id"]}) != len(bindings):
        raise ValueError("selected allocations did not prove distinct worker boot identities")
    return probes


def invalid(phase, reason, **evidence):
    return {"verdict": "invalid", "phase": phase, "reason": reason, "metrics": {}, **evidence}


class WorkerQueue:
    """Track broker occupancy while independent case jobs run on one worker."""

    def __init__(self, control, directory, binding, run_id, worker):
        self.control = control
        self.directory = directory
        self.binding = binding
        self.run_id = run_id
        self.worker = worker
        self.lock = threading.Lock()
        self.active = set()
        self.blocked = None

    def run(self, row):
        job = row["job"]
        output = self.directory / job
        limit = CASES[row["case"]].seconds
        phase = "worker-admission"
        admitted = False
        try:
            with self.lock:
                if self.blocked:
                    return invalid(phase, self.blocked)
                jobs = check_jobs(self.control, self.worker, output.with_name(job + "-jobs.json"), self.active)
                if any(old.get("name") == job for old in jobs):
                    raise ValueError(f"broker job identity already exists: {job}")
                phase = "broker-submit"
                self.active.add(job)
                admitted = True
                try:
                    status = self.control.submit(self.worker, job, worker_args(
                        self.binding, self.run_id, job, "trial", limit + 60, "--case", row["case"]), output)
                except Exception:
                    # The POST may have committed. Do not submit another job on this worker.
                    self.blocked = f"submission of {job} could not be reconciled"
                    raise
            phase = "broker-wait"
            ok = self.control.wait_job(status, output, limit + 125)
            with self.lock:
                self.active.remove(job)
                if not ok:
                    self.blocked = f"worker wrapper failed for {job}"
            phase = "worker-report"
            events = records(output / "logs.json", job)
            if ok and "trial" in events:
                return validated_trial(events["trial"], row)
            reason = events.get("error", {}).get("reason", "broker job ended without a valid worker result")
            status_receipt = json.loads((output / "status.json").read_text())["properties"]
            timed_out = status_receipt.get("ExecMainStatus") in {"124", 124}
            if timed_out and "error" not in events:
                reason = f"broker wrapper exceeded its {limit + 60}s deadline"
            return invalid("broker-timeout" if timed_out else phase, reason,
                           started=events.get("started"), worker_error=events.get("error"))
        except (OSError, RuntimeError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
            if phase in {"worker-admission", "broker-wait"}:
                with self.lock:
                    self.blocked = f"{job} left worker availability unconfirmed"
            events = {}
            if admitted and not (isinstance(error, HTTPError) and error.status == 409):
                collect_logs(self.control, self.worker, job, output)
                try:
                    events = records(output / "logs.json", job)
                except (ValueError, KeyError):
                    pass
            return invalid("broker-timeout" if isinstance(error, (TimeoutError, subprocess.TimeoutExpired))
                           else phase, bounded_reason(error), started=events.get("started"),
                           worker_error=events.get("error"))


def dispatch(control, directory, run_id, bindings, rows, concurrency):
    queues = {worker: WorkerQueue(control, directory / worker, binding, run_id, worker)
              for worker, binding in bindings.items()}
    pools = {worker: ThreadPoolExecutor(max_workers=concurrency) for worker in bindings}
    try:
        pending = [pools[row["worker"]].submit(queues[row["worker"]].run, row) for row in rows]
        return [future.result() for future in pending]
    finally:
        for pool in pools.values():
            pool.shutdown()


def max_overlap(intervals):
    # Monotonic timestamps are comparable only on one worker boot. The end of a
    # half-open interval sorts before a simultaneous start.
    endpoints = []
    for start, end in intervals:
        if end > start:
            endpoints.extend(((start, 1), (end, -1)))
    running = peak = 0
    for _, delta in sorted(endpoints):
        running += delta
        peak = max(peak, running)
    return peak


def normalized(directory, row, result, profile, binding, placement):
    case = CASES[row["case"]]
    outputs = result.get("catalog_fixtures")
    workload = {"source": binding["source_workload"], "suite": binding["suite_workload"],
                "catalog_fixtures": outputs}
    identities_complete = ("worker_environment" in placement
                           and (case.script != "budget" or bool(outputs and all(outputs))))
    paths = [str(directory / name) for name in ("schedule.json", "observed-placement.json", "report.json")]
    if (directory / "preflight.json").exists():
        paths.append(str(directory / "preflight.json"))
    broker = directory / row["worker"] / row["job"]
    if broker.exists():
        paths.append(str(broker))
    paths.extend(result[name] for name in ("evidence_dir", "native_dir") if result.get(name))
    return {
        "schema": compare.SCHEMA, "case": row["case"], "profile": profile,
        "engine": {"name": "af-native", "build": binding["binary_sha256"]},
        "inputs": {"workload_sha256": digest(workload), "fault_sha256": digest(
            {"case": row["case"], "arguments": case.args, "deadline_s": case.seconds}),
            "placement_sha256": digest({"cohort": placement, "worker": row["worker"]}),
            "clock": CLOCK, "seed": row["seed"]},
        "expectation": case.expectation,
        "fault_witnessed": bool(result.get("fault_witness")), "verdict": result["verdict"],
        "violations": result.get("violations", []), "metrics": result["metrics"], "evidence": paths,
        "reason": result["reason"], "workload_evidence_complete": identities_complete,
    }


def totals(rows):
    def measured(values):
        return {"sum": sum(values) if values else None, "measured": len(values),
                "missing": len(rows) - len(values)}

    intervals = [row["interval_ns"] for row in rows if row.get("interval_ns")]
    return {"planned": len(rows), "observed_intervals": len(intervals),
            "incomplete_started_intervals": sum(bool(row.get("started")) and not row.get("interval_ns")
                                                for row in rows),
            "observed_max_overlap": max_overlap(intervals) if intervals else None,
            "spent_case_wall_s": measured([(end - start) / 1e9 for start, end in intervals]),
            "spent_case_cpu_s": measured([row["metrics"]["cpu_s"] for row in rows if "cpu_s" in row["metrics"]]),
            "outcomes": dict(Counter(row["status"] for row in rows))}


def inline(value):
    return str(value).replace("|", "&#124;").replace("\n", " ").replace("<", "&lt;")


def write_reports(directory, profile, workers, rows, elapsed, preflight_seconds, preflight_error):
    report = {"schema": RUN_SCHEMA, "profile": profile, "preflight_error": preflight_error,
              "preflight_s": preflight_seconds, "load_elapsed_s": elapsed,
              "trials_with_measured_intervals_per_min": (
                  sum(bool(row.get("interval_ns")) for row in rows) * 60 / elapsed if elapsed else None),
              "aggregate": {key: value for key, value in totals(rows).items()
                            if key != "observed_max_overlap"},
              "workers": {worker: totals([row for row in rows if row["worker"] == worker])
                          for worker in workers},
              "trials": rows}
    save(directory / "report.json", report)
    lines = [
        "# Native durability load report", "", f"Profile `{profile}`.",
        f"Preflight {preflight_seconds:.2f} s. Load elapsed {elapsed:.2f} s.",
        f"Trial outcomes {report['aggregate']['outcomes']}.",
    ]
    if preflight_error:
        lines.append(f"Preflight refused the cohort. {inline(preflight_error)}")
    throughput = report["trials_with_measured_intervals_per_min"]
    if throughput is not None:
        lines.append(f"Throughput {throughput:.2f} observed case intervals per minute.")
    lines.extend(["", "| Worker | Planned | Intervals | Peak overlap | Case wall s | Case CPU s |",
                  "|---|---:|---:|---:|---:|---:|"])
    for worker, row in report["workers"].items():
        def render(field):
            value = row[field]
            return ("unavailable" if value["sum"] is None
                    else f"{value['sum']:.2f} ({value['measured']}/{row['planned']})")

        lines.append(f"| {worker} | {row['planned']} | {row['observed_intervals']} | "
                     f"{row['observed_max_overlap'] if row['observed_intervals'] else 'unavailable'} | "
                     f"{render('spent_case_wall_s')} | {render('spent_case_cpu_s')} |")
    lines.extend(["", "| Case | Replicate | Worker | Status | Reason |",
                  "|---|---:|---|---|---|"])
    for row in rows:
        lines.append(f"| {row['case']} | {row['replicate']} | {row['worker']} | "
                     f"{row['status']} | {inline(row['reason'])} |")
    lines.extend(["", "Worker counts include failed and invalid trials. Measurement coverage is shown",
                  "in parentheses. See `report.json` for native outcomes, fault receipts, diagnostics,",
                  "invariants, timing and simulated versus real Provider metrics.", ""])
    (directory / "report.md").write_text("\n".join(lines))
    return 0 if all(row["verdict"] in {"pass", "unsupported"} for row in rows) else 1


def run(args, control):
    cases = load_profiles(args.manifest)[args.profile]
    bindings = load_workers(args.workers)
    if (not 1 <= args.repeat <= 32 or not 1 <= args.concurrency <= 8
            or not 0 <= args.seed <= 2 ** 31 - 32 or not len(bindings) <= len(cases) * args.repeat <= 512):
        raise ValueError("require 1–32 repeats, 1–8 slots, a nonnegative 31-bit seed and 1–512 trials covering every worker")
    if not re.fullmatch(r"af-load-[0-9a-f]{20}", args.run_id):
        raise ValueError("run ID must be af-load- followed by 20 lowercase hex digits")
    directory = args.evidence.resolve()
    if directory.is_relative_to(CHECKOUT) or any((anchor / ".git").exists() for anchor in directory.parents):
        raise ValueError("retain evidence outside Git checkouts")
    directory.mkdir(mode=0o700)
    suite_identity = files_identity(SUITE, SUITE_FILES)
    for worker, binding in bindings.items():
        binding["suite_workload"] = suite_identity
        (directory / worker).mkdir(mode=0o700)
    rows = schedule(args.run_id, cases, list(bindings), args.repeat, args.seed)
    placement = {"profile": args.profile, "workers": list(bindings), "cases": cases,
                 "repeat": args.repeat, "concurrency": args.concurrency, "seed": args.seed,
                 "assignments": [(row["case"], row["worker"], row["seed"]) for row in rows]}
    save(directory / "schedule.json", {"schema": "af-durability/load-schedule@1", "run": args.run_id,
                                     "placement": placement, "workers": bindings, "trials": rows})
    preflight_started = time.monotonic()
    preflight_error = None
    try:
        probes = preflight(control, directory, args.run_id, bindings, rows, args.concurrency)
        placement["worker_environment"] = {worker: receipt["environment"] for worker, receipt in probes.items()}
        placement["namespace_available"] = {
            worker: receipt["namespace"]["available"] if receipt["namespace"] else None
            for worker, receipt in probes.items()
        }
    except (OSError, RuntimeError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        preflight_error = bounded_reason(error)
    preflight_seconds = time.monotonic() - preflight_started
    start = time.monotonic()
    results = ([invalid("preflight", preflight_error) for _ in rows] if preflight_error else
               dispatch(control, directory, args.run_id, bindings, rows, args.concurrency))
    elapsed = 0 if preflight_error else time.monotonic() - start
    save(directory / "observed-placement.json", placement)
    normalized_dir = directory / "trials"
    normalized_dir.mkdir()
    for row, result in zip(rows, results, strict=True):
        trial = normalized(directory, row, result, args.profile, bindings[row["worker"]], placement)
        reference = normalized_dir / (row["job"] + ".json")
        save(reference, trial)
        row.update(result, status=STATUS[result["verdict"]],
                   measurements={name: result["metrics"].get(name) for name in METRICS},
                   normalized=str(reference))
    return write_reports(directory, args.profile, bindings, rows, elapsed, preflight_seconds, preflight_error)


def compare_runs(baseline, candidate):
    result = compare.compare(compare.load(baseline / "trials"), compare.load(candidate / "trials"))
    transitions = Counter()
    for row in result["pairs"]:
        before, after = row["before"], row["after"]
        if before and after and not all(
                item.get("workload_evidence_complete", True) for item in (before, after)):
            row["comparison"] = "unavailable_inputs"
            row["delta"] = {}
        transition = row["comparison"]
        if transition == "paired":
            previous, current = before["verdict"], after["verdict"]
            if (previous, current) == ("fail", "pass"):
                transition = "fail-to-pass"
            elif (previous, current) == ("pass", "fail"):
                transition = "regression"
            else:
                transition = f"{previous}-to-{current}"
        row["transition"] = transition
        row["missing_metrics"] = {name: [side for side, record in (("before", before), ("after", after))
                                         if not record or name not in record["metrics"]]
                                  for name in METRICS}
        transitions[transition] += 1
    return {**result, "schema": "af-durability/load-comparison@1", "transitions": dict(transitions)}


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    trial = commands.add_parser("run", help="use existing allocated AF workers")
    trial.add_argument("--manifest", type=Path, default=SUITE / "manifests/load-cases.json")
    trial.add_argument("--profile", required=True)
    trial.add_argument("--workers", type=Path, required=True)
    trial.add_argument("--repeat", type=int, required=True)
    trial.add_argument("--concurrency", type=int, required=True)
    trial.add_argument("--seed", type=int, default=0)
    trial.add_argument("--run-id", default="af-load-" + secrets.token_hex(10))
    trial.add_argument("--connect", help="path to the configured controller SSH helper; default loopback")
    trial.add_argument("--evidence", type=Path, required=True)
    pins = commands.add_parser("pins", help="hash source fixture bytes on the controller")
    pins.add_argument("source", type=Path)
    diff = commands.add_parser("compare", help="compare baseline and candidate suite evidence")
    diff.add_argument("baseline", type=Path)
    diff.add_argument("candidate", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "run":
            status = run(args, Control(args.connect))
            print(args.evidence.resolve() / "report.md")
            return status
        if args.command == "pins":
            print(files_identity(args.source.resolve(strict=True), FIXTURES))
        else:
            print(json.dumps(compare_runs(args.baseline, args.candidate), indent=2))
    except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
