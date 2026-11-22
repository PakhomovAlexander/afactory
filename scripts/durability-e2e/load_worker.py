"""Preflight and execute one private native load trial on a Linux worker."""
import argparse
import ctypes
import errno
import json
import os
import platform
from pathlib import Path
import re
import resource
import shutil
import signal
import subprocess
import sys
import time

from load_cases import CASES, DIGEST_PATTERN, FIXTURES, MARKER, SUITE_FILES, file_sha, files_identity
from load_identity import compute_identity
from native_budget import ADMISSION
from native_recovery import check_identity
from recovery_evidence import before_identity
from native_smoke import private_scratch
from worker_failover import REQUIRED

SUITE = Path(__file__).resolve().parent
GIB = 1024 ** 3
TERMINAL = {"succeeded", "failed", "cancelled"}
ENV = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1",
       "PYTHONPYCACHEPREFIX": "/dev/null", "AF_SELF_OFFLINE": "1", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}


def emit(job, event, value):
    print(MARKER + json.dumps({"job": job, "event": event, **value}, separators=(",", ":")),
          flush=True)


def canonical(value):
    path = Path(value)
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise ValueError(f"path must be absolute and free of aliases: {path}")
    return path


def available(scratch):
    memory = next(int(row.split()[1]) * 1024 for row in Path("/proc/meminfo").read_text().splitlines()
                  if row.startswith("MemAvailable:"))
    fs = os.statvfs(scratch)
    return {"cpus": len(os.sched_getaffinity(0)), "memory_bytes": memory,
            "free_bytes": fs.f_frsize * fs.f_bavail, "free_inodes": fs.f_favail}


def check_capacity(capacity, slots, trials):
    requirements = {"cpus": slots, "memory_bytes": slots * GIB,
                    "free_bytes": slots * GIB + trials * (GIB // 4),
                    "free_inodes": (slots + trials) * 4096}
    for field, minimum in requirements.items():
        if capacity[field] < minimum:
            raise ValueError(f"insufficient {field}: {capacity[field]} available, {minimum} required")


def verify(binding):
    if sys.platform != "linux" or sys.version_info < (3, 11):
        raise ValueError("worker requires Linux and Python 3.11+")
    paths = {key: canonical(binding[key]) for key in ("binary", "source", "suite", "scratch")}
    scratch = private_scratch(paths["scratch"])
    if any(scratch.is_relative_to(path) or path.is_relative_to(scratch)
           for key, path in paths.items() if key != "scratch"):
        raise ValueError("scratch must be disjoint from binary, source and suite")
    # A private leaf under a replaceable parent is unsafe even if it currently has mode 0700.
    for parent in scratch.parents:
        metadata = parent.stat()
        if metadata.st_uid not in {0, os.getuid()} or (
                metadata.st_mode & 0o022 and not metadata.st_mode & 0o1000):
            raise ValueError(f"scratch ancestor can be replaced by another user: {parent}")
    if paths["suite"] != SUITE:
        raise ValueError("worker entry point is outside its bound suite")
    if not paths["binary"].is_file() or not os.access(paths["binary"], os.X_OK):
        raise ValueError("bound binary is not executable")
    git_root, head = subprocess.check_output(
        ["git", "-C", paths["source"], "rev-parse", "--show-toplevel", "HEAD"],
        env=ENV, text=True, timeout=10,
    ).splitlines()
    if Path(git_root) != paths["source"]:
        raise ValueError("bound source must name the root of its Git checkout")
    observed = {"binary_sha256": file_sha(paths["binary"]), "source_commit": head,
                "source_workload": files_identity(paths["source"], FIXTURES),
                "suite_workload": files_identity(SUITE, SUITE_FILES)}
    for key, value in observed.items():
        if binding[key] != value:
            raise ValueError(f"{key} pin mismatch: expected {binding[key]}, observed {value}")
    return scratch


def namespace_probe(root):
    probe = root / "namespace-probe"
    probe.mkdir(mode=0o700)
    result = subprocess.run(
        ["/usr/bin/unshare", "--user", "--map-root-user", "--mount", "--fork", "--kill-child",
         "/usr/bin/mount", "-t", "tmpfs", "-o", "size=4M,nosuid,nodev,noexec", "tmpfs", probe],
        capture_output=True, text=True, env=ENV, timeout=10,
    )
    diagnostic = result.stderr.strip()[-1000:]
    if result.returncode and not re.search(r"operation not permitted|permission denied|must be superuser",
                                           diagnostic, re.IGNORECASE):
        raise RuntimeError(f"namespace probe exited {result.returncode}: {diagnostic}")
    return {"available": result.returncode == 0, "exit": result.returncode, "diagnostic": diagnostic}


def preflight(binding, args):
    scratch = verify(binding)
    for name in ("git", "timeout"):
        if not shutil.which(name, path=ENV["PATH"]):
            raise ValueError(f"worker is missing {name}")
    capacity = available(scratch)
    check_capacity(capacity, args.slots, args.trials)
    # Timeout cleanup needs process handles that cannot refer to a recycled PID.
    os.close(os.pidfd_open(os.getpid()))
    root = scratch / args.run
    root.mkdir(mode=0o700)
    disk = namespace_probe(root) if args.namespace else None
    filesystem = subprocess.check_output(["stat", "-f", "-c", "%T", scratch], env=ENV, text=True,
                                         timeout=5).strip()
    result = {"schema": "af-durability/load-preflight@2",
              "environment": {"compute_identity": compute_identity(), "cpus": capacity["cpus"], "arch": platform.machine(),
                              "kernel": platform.release(), "python": platform.python_version(),
                              "scratch": str(scratch), "filesystem": filesystem},
              "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
              "namespace": disk, "capacity": capacity, "evidence_dir": str(root),
              "scratch_device": scratch.stat().st_dev, "scratch_inode": scratch.stat().st_ino,
              "pins": {name: binding[name] for name in ("binary_sha256", "source_commit",
                                                        "source_workload", "suite_workload")}}
    (root / "preflight.json").write_text(json.dumps(result) + "\n")
    return result


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("ambiguous duplicate JSON key")
        value[key] = item
    return value


def read(path, *, unique=False):
    try:
        with path.open("rb") as stream:
            data = stream.read(4 * 1024 * 1024 + 1)
        if len(data) <= 4 * 1024 * 1024:
            value = json.loads(data, object_pairs_hook=unique_object if unique else None)
            return value if isinstance(value, dict) else {}
    except (OSError, ValueError, RecursionError):
        pass
    return {}


def tail(path, limit=3000):
    if not path.is_file():
        return ""
    with path.open("rb") as stream:
        stream.seek(max(0, path.stat().st_size - limit))
        return stream.read(limit).decode(errors="replace").strip()


def before_check(before):
    """Read the one interrupted native recovery action, independently of Checks."""
    try:
        revision, intent_id = before_identity(before)
        return check_identity("pagination", revision), revision, intent_id
    except (KeyError, TypeError, ValueError, AttributeError, IndexError, OverflowError, RecursionError):
        return None


def checks_fault_witness(case_id, ledger, before):
    expected = before_check(before)
    if expected is None:
        return False
    check_id, revision, expected_intent = expected
    checks, calls = ledger.get("checks"), ledger.get("calls")
    if (not isinstance(checks, list) or len(checks) != 1 or not isinstance(checks[0], dict)
            or not isinstance(calls, list)):
        return False
    digest = "sha256:" + DIGEST_PATTERN
    if checks[0] != {"id": check_id, "revision": revision}:
        return False

    stale = "sha256:" + "0" * 64
    if case_id == "wrong-revision" and revision == stale:
        return False
    create, read_fault = {
        "restart-green": ((0, None), (503, None)),
        "restart-red": ((0, None), (503, None)),
        "expired-auth": ((201, revision), (401, None)),
        "wrong-revision": ((201, revision), (200, stale)),
    }[case_id]
    sequence = 0
    created = set()
    witnessed = False
    statuses = {"create": (0, 200, 201, 400, 401, 409), "read": (200, 401, 404, 503)}
    for call in calls:
        if not isinstance(call, dict) or call.get("check_id") != check_id:
            return False
        number, intent = call.get("sequence"), call.get("intent")
        action, status = call.get("action"), call.get("status")
        if (type(number) is not int or number <= sequence or not isinstance(intent, str)
                or not re.fullmatch(digest, intent) or intent != expected_intent or type(status) is not int
                or not isinstance(action, str) or status not in statuses.get(action, ())
                or "served_revision" not in call):
            return False
        sequence = number
        served = call["served_revision"]
        if status in (200, 201):
            if not isinstance(served, str) or not re.fullmatch(digest, served):
                return False
        elif served is not None:
            return False
        if action == "create" and (status, served) == create:
            created.add(intent)
        if action == "read" and (status, served) == read_fault and intent in created:
            witnessed = True
    return witnessed


def witness(case_id, root):
    """Return a receipt only when it proves the declared fault happened."""
    if case_id == "gate-red":
        checks = read(root / "task-checks.json").get("receipt") or {}
        if any(type(check.get("exit_code")) is int for check in checks.get("checks", [])):
            return "task-checks.json"
    if case_id in {"restart-green", "restart-red", "expired-auth", "wrong-revision"}:
        crash = read(root / "crash.json")
        exit_code = crash.get("exit")
        if (type(exit_code) is int and exit_code == -signal.SIGKILL and crash.get("fixture_alive") is True
                and checks_fault_witness(case_id, read(root / "ledger-before.json"),
                                         read(root / "before.json", unique=True))):
            return "before.json + ledger-before.json + crash.json"
    if case_id == "journal-enospc":
        disk = read(root / "disk-fault.json")
        if disk.get("pressure_errno") == errno.ENOSPC and disk.get("af_reported_full_store"):
            return "disk-fault.json"
    if case_id == "artifact-enospc" and read(root / "cas-fault.json").get("errno") == errno.ENOSPC:
        return "cas-fault.json"
    if case_id == "writer-loss" and read(root / "writer-loss.json").get("signal") == 9:
        return "writer-loss.json"
    if case_id == "child-cancel" and read(root / "outage.json").get("stopped_signal"):
        if read(root / "cancel.json").get("request", {}).get("task_id"):
            return "outage.json + cancel.json"
    if case_id in {"admission", "pause"}:
        names = ("initial-ant.json", "paused-after-restart-ant.json")
        receipts = [read(root / name).get("occurrences", []) for name in names]
        busy = any(row["outcome"]["kind"] == "skipped_busy" for row in receipts[0])
        paused = any(row["outcome"]["kind"] == "skipped_paused" for row in receipts[1])
        if busy and (case_id == "admission" or paused):
            return names[0] if case_id == "admission" else " + ".join(names)
    if case_id == "gate-repair":
        checks = read(root / "first-checks.json").get("receipt") or {}
        if any(check.get("exit_code") == 1 for check in checks.get("checks", [])):
            return "first-checks.json"
    if case_id in {"budget", "ambiguous-charge", "hostile-output", "malformed-usage"}:
        ledger = read(root / "provider-evidence.json")
        operations = {row["id"]: row["operation"] for row in ledger.get("calls", [])}
        for event in ledger.get("events", []):
            op = operations.get(event["call"])
            detail = event["detail"]
            if ((case_id == "budget" and op == "exhausted-admission" and event["kind"] == "charged"
                 and detail.get("input_tokens", 0) > ADMISSION)
                    or (case_id == "ambiguous-charge" and event["kind"] == "fault_fired"
                        and detail.get("boundary") == "after_charge")
                    or (case_id in {"hostile-output", "malformed-usage"} and op == "exhausted-author"
                        and event["kind"] == "response_written")):
                return "provider-evidence.json"
    return None


def outcomes(root, case, summary):
    """Extract concise native outcomes while retaining complete evidence on the VM."""
    scenarios = {}
    tasks = {}
    violations = []
    native_paths = {
        "smoke": ("observed.json", "task.json"),
        "gate_repair": ("observed.json", "first-task.json", "repair-task.json"),
        "recovery": ("after.json", "task-after.json"),
        "artifact_fault": ("after.json", "at-full.json", "task-after.json"),
        "writer_loss": ("terminal.json", "after-late.json", "task-after.json"),
        "child_cancel": ("final.json", "task-cancelled.json", "task-after.json", "next-task.json"),
        "admission": ("recovered-runs.json", "paused-after-restart-runs.json",
                      "admitted-task-0.json", "admitted-task-1.json"),
        "budget": ("exhausted/observed.json", "funded/observed.json", "exhausted/task.json", "funded/task.json"),
    }.get(case.script, ())
    for name in native_paths:
        value = read(root / name)
        runs = value.get("runs", {name: value})
        for run_id, run in runs.items():
            if "status" not in run or "frames" not in run:
                continue
            steps = {step["step"]: step["status"] for step in run["frames"]["root"]["steps"]}
            scenarios[f"{name}:{run_id}"] = {"status": run["status"], "steps": steps,
                                             "failure": str(run.get("failure") or "")[:500]}
            for step, status in steps.items():
                if run["status"] in TERMINAL and status == "running":
                    problem = f"terminal Scenario has running step {step}"
                    if problem not in violations:
                        violations.append(problem)
            for step in run["frames"]["root"]["steps"]:
                port = (step.get("outputs") or {}).get("result", {})
                outcome = port.get("value", {})
                task_id = (outcome.get("receipt") or {}).get("task_id") if isinstance(outcome, dict) else None
                if task_id:
                    tasks[task_id] = {"outcome": outcome.get("outcome"), "cause": outcome.get("cause")}
            for task in run.get("fresh_tasks", {}).values():
                if "task_id" in task:
                    tasks.setdefault(task["task_id"], {"outcome": None})
        if "state" in value and isinstance(value["state"], dict) and "outcome" in value["state"]:
            tasks[value.get("task_id", name)] = value["state"]
    for field in ("task", "task_state", "child_task", "cancelled_child_before_restart",
                  "cancelled_child_after_restart"):
        if field in summary:
            tasks[field] = summary[field]
    if not scenarios and "scenario" in summary:
        scenarios["summary"] = {"status": summary["scenario"]}
    return scenarios, tasks, violations


def judge(case_id, root, exit_code, timed_out, diagnostic):
    case = CASES[case_id]
    summary = read(root / case.summary)
    receipt = witness(case_id, root)
    scenarios, tasks, violations = outcomes(root, case, summary)
    invariants = summary.get(case.checks, {}) if case.checks else {}
    violations.extend(key for key, value in invariants.items() if value is False)
    if case_id == "gate-red" and receipt:
        checks = read(root / receipt)["receipt"]["checks"]
        if not any(check.get("exit_code") == 1 for check in checks):
            violations.append("required red gate did not produce its expected exit code")
    if summary.get("terminal_steps_settled") is False:
        violations.append("terminal_steps_settled")
    complete = bool(summary) and (
        (bool(invariants) and all(type(value) is bool for value in invariants.values()))
        if case.checks else {"scenario_status", "task_state", "attempts"} <= summary.keys()
    )
    if timed_out:
        verdict, phase, reason = "invalid", "case-timeout", diagnostic
    elif case.expectation != "control" and not receipt:
        verdict, phase, reason = "invalid", "before-fault-witness", diagnostic or "missing required fault receipt"
    elif violations or (complete and exit_code == 1):
        verdict, phase, reason = "fail", "verify-outcome", ", ".join(violations) or "native expected outcome failed"
        if not violations:
            violations.append(reason)
    elif not complete or exit_code != 0:
        phase = "after-fault-witness" if receipt else "native-case"
        verdict, reason = "invalid", diagnostic or f"missing complete {case.summary}"
    else:
        verdict, phase, reason = "pass", "verified", "native outcome and invariants satisfied"
    metrics = {}
    ledger = read(root / "provider-evidence.json")
    if ledger.get("events"):
        charges = [row["detail"] for row in ledger["events"] if row["kind"] == "charged"]
        if charges:
            metrics["simulated_tokens"] = sum(row["input_tokens"] + row["output_tokens"] for row in charges)
        if complete:
            metrics["unknown_settlements"] = sum(len(summary[name]["unknown_usage"])
                                                 for name in ("exhausted", "funded"))
    # These runners use only the loopback mock provider or command Workers.
    # Zero is established for a completed native runner, not inferred for missing evidence.
    if complete:
        metrics["provider_tokens"] = 0
    fixture_digests = None
    if case.script == "budget":
        fixture_digests = [read(root / name / "catalog-fixture.json").get("sha256")
                           for name in ("exhausted", "funded")]
    timings = {key: value for key, value in summary.items()
               if key.endswith("_s") and type(value) in (int, float)}
    accounting = {name: summary[name] for name in ("attempts", "chargeable_tokens",
                                                   "known_chargeable_tokens", "http_calls",
                                                   "provider_calls", "occurrences") if name in summary}
    if case.script == "budget":
        for name in ("exhausted", "funded"):
            if name in summary:
                accounting[name] = {key: summary[name][key] for key in ("attempts", "chargeable_tokens", "budget")}
    return {"verdict": verdict, "phase": phase, "reason": reason, "diagnostic": diagnostic,
            "fault_witness": receipt, "violations": violations, "invariants": invariants,
            "scenarios": scenarios, "tasks": tasks, "native_timings": timings, "metrics": metrics,
            "summary": case.summary if summary else None, "catalog_fixtures": fixture_digests,
            "native_accounting": accounting}


def adopt_children():
    # The existing native fixtures start schedulers in separate process groups.
    # Make this worker the reaper for its own orphaned descendants.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code))


def stop_orphans():
    children = Path(f"/proc/self/task/{os.getpid()}/children")
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        pids = children.read_text().split()
        if not pids:
            return
        for entry in pids:
            pid = int(entry)
            try:
                fd = os.pidfd_open(pid)
                try:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                finally:
                    os.close(fd)
            except ProcessLookupError:
                pass
            try:
                os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                pass
        time.sleep(0.05)
    raise RuntimeError("private native descendants did not exit within 5s")


def execute(argv, output, env, root, seconds):
    timed_out = False
    with output.open("wb") as log:
        start = time.monotonic_ns()
        try:
            result = subprocess.run(argv, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT,
                                    timeout=seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
        finally:
            # Case timing ends at the subprocess boundary, before descendant cleanup.
            end = time.monotonic_ns()
            if sys.platform == "linux":
                stop_orphans()
    code = None if timed_out else result.returncode
    diagnostic = (f"native process exceeded {seconds}s deadline" if timed_out else
                  f"native process exited {code}" if code else "")
    if diagnostic:
        diagnostic += "\n" + tail(output)
    return code, timed_out, diagnostic, [start, end]


def run_case(binding, args):
    scratch = verify(binding)
    root = scratch / args.run
    preflight_result = read(root / "preflight.json")
    if (preflight_result.get("boot_id") != Path("/proc/sys/kernel/random/boot_id").read_text().strip()
            or preflight_result.get("scratch_device") != scratch.stat().st_dev
            or preflight_result.get("scratch_inode") != scratch.stat().st_ino
            or canonical(root) != root):
        raise ValueError("worker or scratch changed after preflight")
    check_capacity(available(scratch), 1, 1)
    job_dir = root / args.job
    job_dir.mkdir(mode=0o700)
    home = job_dir / "home"
    home.mkdir(mode=0o700)
    env = {**ENV, "HOME": str(home), "TMPDIR": str(job_dir), "AF_DURABILITY_SCRATCH": str(job_dir)}
    case = CASES[args.case]
    output = job_dir / "native.log"
    if case.namespace and not preflight_result["namespace"]["available"]:
        return {"verdict": "unsupported", "phase": "namespace-probe",
                "reason": preflight_result["namespace"]["diagnostic"],
                "evidence_dir": str(job_dir), "interval_ns": None, "metrics": {}}
    adopt_children()
    argv = [sys.executable, "-B", str(SUITE / f"native_{case.script}.py")]
    if args.case == "independent-writer":
        manifest = read(SUITE / "manifests/worker-failover.json")
        completed = subprocess.run([*argv, "capabilities"], input=json.dumps({"input": manifest}),
                                   cwd=job_dir, env=env, capture_output=True, text=True, timeout=case.seconds)
        output.write_text(completed.stdout + completed.stderr)
        completed.check_returncode()
        probe = json.loads(completed.stdout)
        missing = REQUIRED - set(probe["capabilities"])
        if probe["requested_profile"] != manifest["profile"] or not missing or not probe["reason"]:
            raise ValueError("native probe did not verify a missing independent-writer capability")
        (job_dir / "capabilities.json").write_text(json.dumps(probe) + "\n")
        return {"verdict": "unsupported", "phase": "capability-probe", "reason": probe["reason"],
                "missing_capabilities": sorted(missing), "evidence_dir": str(job_dir),
                "interval_ns": None, "metrics": {}}
    emit(args.job, "started", {"case": args.case, "evidence_dir": str(job_dir)})
    cpu = resource.getrusage(resource.RUSAGE_CHILDREN)
    code, timed_out, diagnostic, interval = execute(
        [*argv, *case.arguments(binding["binary"], binding["source"])], output, env, job_dir, case.seconds)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    roots = [path for path in job_dir.glob("af-durability-*") if path.is_dir()]
    if len(roots) > 1:
        raise ValueError("native runner created more than one evidence directory")
    native_root = roots[0] if roots else job_dir
    try:
        result = judge(args.case, native_root, code, timed_out, diagnostic)
    except (ValueError, TypeError, KeyError, AttributeError) as error:
        result = {"verdict": "invalid", "phase": "worker-evidence",
                  "reason": f"could not interpret native evidence: {error}",
                  "diagnostic": diagnostic, "summary": None, "metrics": {}}
    result["metrics"]["cpu_s"] = (after.ru_utime - cpu.ru_utime) + (after.ru_stime - cpu.ru_stime)
    if result["verdict"] in {"pass", "fail"} and result["summary"]:
        result["metrics"]["terminal_s"] = (interval[1] - interval[0]) / 1e9
    return {**result, "interval_ns": interval, "evidence_dir": str(job_dir),
            "native_dir": str(native_root), "native_exit": code}


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("preflight", "trial"))
    parser.add_argument("binding")
    parser.add_argument("run")
    parser.add_argument("job")
    parser.add_argument("--case", choices=CASES)
    parser.add_argument("--slots", type=int)
    parser.add_argument("--trials", type=int)
    parser.add_argument("--namespace", action="store_true")
    args = parser.parse_args()
    if (not re.fullmatch(r"af-load-[0-9a-f]{20}", args.run)
            or not re.fullmatch(re.escape(args.run) + r"-(probe|[0-9]{4})", args.job)):
        parser.error("invalid run or job identity")
    try:
        binding = json.loads(args.binding)
        identity = compute_identity() if args.phase == "trial" else None
        result = preflight(binding, args) if args.phase == "preflight" else run_case(binding, args)
        if args.phase == "trial":
            result["compute_identity"] = identity if identity == compute_identity() else None
    except (Exception, SystemExit) as error:
        emit(args.job, "error", {"phase": args.phase,
                                 "reason": (f"{type(error).__name__}: {error}")[-3500:]})
        return 1
    emit(args.job, args.phase, result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
