"""Run bounded durability jobs through the configured controller SSH entry point."""
import argparse
import base64
import json
import os
import re
import shlex
import subprocess
import time
import zlib
from pathlib import Path

ORIGIN = "http://127.0.0.1:8877"


class HTTPError(RuntimeError):
    def __init__(self, path, status, response):
        super().__init__(f"{path}: HTTP {status} {response.strip()}")
        self.status = status


class Control:
    def __init__(self, connect):
        self.connect = connect

    def request(self, path, body=None):
        if not path.startswith("/") or path.startswith("//"):
            raise ValueError(f"expected a relative control API path: {path!r}")
        argv = ["curl", "--silent", "--show-error", "--write-out", "\\n%{http_code}",
                "--max-time", "45", ORIGIN + path]
        if body is not None:
            argv.extend(["--json", json.dumps(body)])
        if self.connect:
            argv = [self.connect, shlex.join(argv)]
        result = subprocess.run(argv, capture_output=True,
                                text=True, timeout=60)
        if result.returncode:
            raise RuntimeError(f"{path}: {result.stderr.strip()}")
        response, _, status = result.stdout.rpartition("\n")
        if not status.startswith("2"):
            raise HTTPError(path, int(status), response)
        return json.loads(response)

    def submit(self, worker, name, argv, evidence):
        if not re.fullmatch(r"r-[1-9][0-9]{0,7}", worker):
            raise ValueError(f"invalid allocation handle: {worker!r}")
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,48}", name):
            raise ValueError(f"invalid job name: {name!r}")
        prefix = f"/v1/resources/{worker}/jobs"
        status_path = f"{prefix}/{name}"
        request = {"name": name, "argv": argv}
        evidence.mkdir(parents=True, exist_ok=False)
        save(evidence / "request.json", {"worker": worker, **request})
        try:
            receipt = self.request(prefix, request)
        except (RuntimeError, subprocess.TimeoutExpired) as failure:
            save(evidence / "submission-error.json", {"error": str(failure)})
            # An ambiguous acknowledgement may mean the job is already running.
            if isinstance(failure, HTTPError) and failure.status != 503:
                raise
            self.request(status_path)
        else:
            save(evidence / "submission.json", receipt)
            if receipt["status"] != status_path or receipt["logs"] != status_path + "/logs":
                raise RuntimeError("control API returned unexpected job links")
        return status_path

    def wait_job(self, status_path, evidence, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                status = self.request(status_path)
            except HTTPError as error:
                if error.status != 503:
                    raise
                time.sleep(2)
                continue
            save(evidence / "status.json", status)
            properties = status["properties"]
            if (properties["ActiveState"] in {"failed", "inactive"}
                    or properties["SubState"] == "exited"):
                save(evidence / "logs.json", self.request(status_path + "/logs"))
                return (
                    properties.get("Result") == "success"
                    and properties.get("ExecMainCode") == "1"
                    and properties.get("ExecMainStatus") == "0"
                )
            time.sleep(2)
        raise TimeoutError(f"job remains pending at {status_path}")

    def job(self, worker, name, argv, evidence, seconds):
        status_path = self.submit(worker, name, argv, evidence)
        return self.wait_job(status_path, evidence, seconds)


def save(path, value):
    pending = path.with_suffix(".tmp")
    pending.write_text(json.dumps(value, indent=2) + "\n")
    pending.replace(path)


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--connect", required=True, help="path to the pinned control SSH helper")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("inventory")
    plan = commands.add_parser("plan")
    plan.add_argument("task")
    plan.add_argument("vcpus", type=int)
    plan.add_argument("ram_gib", type=int)
    plan.add_argument("ssd_gib", type=int)
    run = commands.add_parser("job")
    run.add_argument("--evidence", required=True, type=Path)
    run.add_argument("--seconds", type=int, default=120)
    run.add_argument("worker")
    run.add_argument("name")
    run.add_argument("argv", nargs=argparse.REMAINDER)
    smoke = commands.add_parser("native-smoke", help="run a provider-free native Scenario")
    smoke.add_argument("--work-base", required=True, help="private scratch directory on the worker")
    smoke.add_argument("--gate", choices=("green", "red"), default="green")
    smoke.add_argument("--evidence", required=True, type=Path)
    smoke.add_argument("worker")
    smoke.add_argument("name")
    smoke.add_argument("binary")
    smoke.add_argument("source")
    args = parser.parse_args()
    control = Control(args.connect)
    if args.command == "inventory":
        print(json.dumps({
            "guide": control.request("/"),
            "capacity": control.request("/v1/capacity"),
            "resources": control.request("/v1/resources"),
        }, indent=2))
    elif args.command == "plan":
        request = {name: getattr(args, name) for name in ("task", "vcpus", "ram_gib", "ssd_gib")}
        print(json.dumps(control.request("/v1/plan", request), indent=2))
    elif args.command == "native-smoke":
        if not all(Path(path).is_absolute() for path in (args.binary, args.source, args.work_base)):
            smoke.error("binary, source and scratch paths must be absolute worker paths")
        source = Path(__file__).with_name("native_smoke.py").read_bytes()
        payload = base64.urlsafe_b64encode(zlib.compress(source)).decode("ascii")
        # The broker caps request size. The alphabet also avoids systemd argv expansion.
        loader = (
            "import base64,sys,zlib\n"
            "source=zlib.decompress(base64.urlsafe_b64decode(sys.argv.pop(1)))\n"
            "exec(compile(source, 'native_smoke.py', 'exec'))"
        )
        argv = ["/usr/bin/timeout", "110", "/usr/bin/python3", "-c", loader,
                payload, args.binary, args.source, args.gate, "--work-base", args.work_base]
        return 0 if control.job(args.worker, args.name, argv, args.evidence, 165) else 1
    else:
        if not args.argv:
            run.error("missing job argv")
        return 0 if control.job(args.worker, args.name, args.argv, args.evidence, args.seconds) else 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
