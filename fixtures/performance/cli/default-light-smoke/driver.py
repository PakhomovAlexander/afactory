#!/usr/bin/env python3
"""Run and inspect the real three-Task default-light development-flow smoke workload."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import sys
import time
from types import SimpleNamespace

FIXTURE = Path(__file__).resolve().parent
TASKS = ("smoke-0", "smoke-1", "smoke-2")
EXPECTED_EXITS = (3, 3, 0)
MAX_STREAM_BYTES = 262144
MAX_TOTAL_OUTPUT_BYTES = 1048576
MAX_EVIDENCE_BYTES = 1048576
INJECTION_ENV = "AF_PERF_CALIBRATION_ITERATIONS"
ITERATIONS_MAX = 1 << 40


def calibration_iterations(value):
    """Match the kernel's canonical, bounded calibration control; absence stays distinct."""
    if value is None:
        return None
    if (re.fullmatch(r"0|[1-9][0-9]{0,13}", value) is None
            or int(value) > ITERATIONS_MAX):
        raise ValueError(f"{INJECTION_ENV} is not a canonical count within 0..={ITERATIONS_MAX}")
    return int(value)


def read_bounded(path, limit=MAX_EVIDENCE_BYTES):
    """Read one regular file, refusing overflow and every symlink component."""
    parts = Path(path).absolute().parts
    descriptor = os.open(parts[0], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in parts[1:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                            dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        child = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                        dir_fd=descriptor)
        os.close(descriptor)
        descriptor = child
        stat = os.fstat(descriptor)
        if not stat.st_mode & 0o170000 == 0o100000:
            raise ValueError(f"not a regular evidence file: {path}")
        if stat.st_size > limit:
            raise ValueError(f"evidence exceeds {limit} bytes: {path}")
        chunks = []
        remaining = limit + 1
        while remaining:
            chunk = os.read(descriptor, min(65536, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        if len(data) > limit:
            raise ValueError(f"evidence exceeds {limit} bytes: {path}")
        return data
    finally:
        os.close(descriptor)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def directory_bytes(path):
    if not path.exists():
        return 0
    return sum(entry.stat().st_size for entry in path.rglob("*") if entry.is_file())


def file_sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def fixture_digest():
    entries = []
    for path in sorted(FIXTURE.rglob("*")):
        if path.is_file() and "__pycache__" not in path.parts:
            entries.append((str(path.relative_to(FIXTURE)), file_sha256(path)))
    canonical = "\n".join(f"{path}:{digest}" for path, digest in entries).encode()
    return "sha256:" + hashlib.sha256(canonical).hexdigest()


def copy_fixture_tree(source, target):
    """Copy authority files as private writable files, preserving only executability."""
    for path in source.rglob("*"):
        relative = path.relative_to(source)
        destination = target / relative
        if path.is_dir():
            destination.mkdir(parents=True, exist_ok=True)
            os.chmod(destination, 0o700)
        elif path.is_file():
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, destination)
            os.chmod(destination, 0o700 if os.access(path, os.X_OK) else 0o600)


class Driver:
    def __init__(self, args):
        self.af = str(Path(args.af).resolve())
        self.work = Path(args.work).resolve()
        self.output = Path(args.output).resolve()
        self.cache_seed = Path(args.cache_seed).resolve() if args.cache_seed else None
        self.prime_only = args.prime_only
        self.primed = args.primed or bool(self.cache_seed)
        self.telemetry = args.telemetry
        self.rustup_home = args.rustup_home
        self.cargo_bin = args.cargo_bin
        self.home = self.work / "home"
        self.cache = self.home / "cache"
        self.store = self.work / "store"
        self.repo = self.work / "repo"
        self.evidence = self.work / "evidence"
        self.commands = []
        self.env = os.environ.copy()
        # The driver owns this dose. No nested af (including failure diagnostics) may
        # reject the control or run its own feature-enabled calibration loop.
        self.iterations = calibration_iterations(self.env.pop(INJECTION_ENV, None))
        self.host = {
            "platform": __import__("platform").platform(),
            "cpu_count": os.cpu_count(),
            "af_binary_sha256": file_sha256(self.af),
            "fixture_sha256": fixture_digest(),
        }

    def prepare(self):
        if self.evidence.exists() or self.repo.exists():
            raise SystemExit(f"work was already used: {self.work}")
        self.work.mkdir(parents=True, exist_ok=True)
        self.evidence.mkdir(parents=True)
        os.chmod(self.work, 0o700)
        for directory in (self.home / "config" / "af", self.home / "state",
                          self.home / "data", self.cache, self.work / "tmp"):
            directory.mkdir(parents=True, exist_ok=True)
        for directory in (self.home, self.home / "config", self.home / "config" / "af",
                          self.home / "state", self.home / "data", self.cache,
                          self.work / "tmp"):
            os.chmod(directory, 0o700)
        if self.cache_seed:
            shutil.copytree(self.cache_seed, self.cache, dirs_exist_ok=True)
        copy_fixture_tree(FIXTURE / "repo", self.repo)
        auth = self.home / "codex-home"
        auth.mkdir(mode=0o700)
        registry = {
            "version": 4,
            "providers": [{"id": "codex-main", "kind": "codex", "auth_dir": str(auth)}],
        }
        toml = 'version = 4\n[[providers]]\nid = "codex-main"\nkind = "codex"\nauth_dir = %s\n' % (
            json.dumps(str(auth)))
        registry_path = self.home / "config" / "af" / "providers.toml"
        registry_path.write_text(toml, encoding="utf-8")
        os.chmod(registry_path, 0o600)
        self.env.update({
            "HOME": str(self.home),
            "XDG_CONFIG_HOME": str(self.home / "config"),
            "XDG_STATE_HOME": str(self.home / "state"),
            "XDG_CACHE_HOME": str(self.cache),
            "XDG_DATA_HOME": str(self.home / "data"),
            "TMPDIR": str(self.work / "tmp"),
            "CODEX_HOME": str(auth),
            "PATH": str(FIXTURE / "mock" / "bin") + os.pathsep + self.cargo_bin + os.pathsep + self.env["PATH"],
            "PYTHONDONTWRITEBYTECODE": "1",
            "CARGO_NET_OFFLINE": "true",
            "CARGO_TERM_COLOR": "never",
            "RUSTUP_HOME": self.rustup_home,
            "AF_STORAGE__MIN_FREE_BYTES": "268435456",
            "AF_TELEMETRY__LEVEL": self.telemetry,
        })
        git_env = self.env.copy()
        git_env.update({
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_AUTHOR_NAME": "fixture",
            "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
            "GIT_COMMITTER_NAME": "fixture",
            "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
            "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
            "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
        })
        for argv in (["init", "-q", "-b", "main"], ["add", "-A"],
                     ["commit", "-q", "-m", "development-flow smoke fixture"]):
            subprocess.run(["git", "-C", str(self.repo)] + argv, env=git_env,
                           check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def inject_calibration(self):
        """Compile once in the timed driver, without changing Task source or Build Caches.

        Rust's constant evaluator performs exactly the declared number of steps and
        embeds the result in a real rlib. Source and artifact sizes stay bounded; there
        is no runtime loop, generated source proportional to the dose, or Cargo cache.
        The sample supervisor still owns the process group, deadline and scratch bound.
        """
        if self.iterations is None:
            return
        injected_ns = 0
        if self.iterations:
            directory = self.work / "calibration"
            directory.mkdir()
            source = directory / "injection.rs"
            source.write_text(
                "#![allow(long_running_const_eval)]\n"
                "pub static CALIBRATION: u64 = {\n"
                "    let mut state = 0x9e3779b97f4a7c15_u64;\n"
                "    let mut step = 0_u64;\n"
                f"    while step < {self.iterations} {{\n"
                "        state = state.wrapping_mul(6364136223846793005).wrapping_add(step | 1);\n"
                "        step += 1;\n"
                "    }\n"
                "    state\n"
                "};\n", encoding="utf-8")
            _, injected_ns = self.invoke(
                "calibration-rustc",
                ["rustc", "--crate-name", "af_perf_calibration", "--crate-type", "rlib",
                 "--edition=2024", "-Copt-level=0", str(source),
                 "-o", str(directory / "injection.rlib")],
                env=dict(self.env, RUSTUP_TOOLCHAIN="1.88.0"), record=False)
        sys.stderr.write(
            f"af.perf-calibration-injection/1 iterations={self.iterations} "
            f"injected_ns={injected_ns}\n")

    def invoke(self, label, argv, expected=0, env=None, record=True):
        started = time.monotonic_ns()
        if any(isinstance(argument, list) for argument in argv):
            raise TypeError(f"nested command argument in {label}: {argv!r}")
        argv = [os.fspath(argument) for argument in argv]
        environment = (self.env if env is None else env).copy()
        environment.pop(INJECTION_ENV, None)
        streams = {}
        with selectors.DefaultSelector() as selector, subprocess.Popen(
                argv, cwd=os.fspath(self.repo), env=environment,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE) as process:
            try:
                for name, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ, name)
                    streams[name] = {"buffer": bytearray(), "overflow": False}
                total = 0
                while selector.get_map():
                    for key, _ in selector.select():
                        name = key.data
                        stream = streams[name]
                        chunk = os.read(key.fileobj.fileno(), 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        total += len(chunk)
                        room = MAX_STREAM_BYTES - len(stream["buffer"])
                        if room > 0:
                            stream["buffer"].extend(chunk[:room])
                        if len(stream["buffer"]) == MAX_STREAM_BYTES or total > MAX_TOTAL_OUTPUT_BYTES:
                            stream["overflow"] = True
                returncode = process.wait()
            except BaseException:
                # A setup/read failure must not leave a child blocked on an undrained pipe.
                # Reap before propagating interrupts too; Popen's context closes both streams.
                process.kill()
                process.wait()
                raise
        elapsed_ns = time.monotonic_ns() - started
        index = len(self.commands)
        for name, stream in streams.items():
            prefix = f"command-{index:02d}-{label}" if record else label
            (self.evidence / f"{prefix}.{name}").write_bytes(stream["buffer"])
        overflow = any(stream["overflow"] for stream in streams.values())
        command = {
            "index": index,
            "label": label,
            "argv": argv,
            "exit_code": returncode,
            "elapsed_ns": elapsed_ns,
            "output_overflow": overflow,
        }
        if record:
            self.commands.append(command)
        if overflow:
            raise RuntimeError(f"{label}: nested output exceeded the laboratory bounds")
        result = SimpleNamespace(
            stdout=streams["stdout"]["buffer"].decode("utf-8", "replace"),
            stderr=streams["stderr"]["buffer"].decode("utf-8", "replace"),
            returncode=returncode,
        )
        if result.returncode != expected:
            raise RuntimeError(
                f"{label}: expected exit {expected}, got {result.returncode}\n"
                f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}")
        return result, elapsed_ns

    def af_args(self, *args):
        return [self.af, *args, "--repo", str(self.repo), "--state", str(self.store), "--json"]

    def json_invoke(self, label, *args, expected=0):
        result, elapsed = self.invoke(label, self.af_args(*args), expected)
        try:
            return json.loads(result.stdout), elapsed
        except json.JSONDecodeError as error:
            raise RuntimeError(f"{label}: invalid JSON: {result.stdout[:1000]}") from error

    def preserve_cargo(self, round_id):
        matches = list(self.cache.glob(
            "af/task-build-cache/*/*/cargo_target/dev-flow-cargo.jsonl"))
        if len(matches) != 1:
            raise RuntimeError(f"expected one Cargo artifact stream, found {matches}")
        records = []
        for line in read_bounded(matches[0]).decode("utf-8").splitlines():
            message = json.loads(line)
            if message.get("reason") != "compiler-artifact":
                continue
            package = message["package_id"].split()[0].rsplit("/", 1)[-1].split("#", 1)[0]
            target = message["target"]
            profile = message["profile"]
            records.append({
                "package": package,
                "target": target["name"],
                "test_profile": profile["test"],
                "fresh": message["fresh"],
            })
        destination = self.evidence / f"round-{round_id}-cargo.jsonl"
        shutil.copyfile(matches[0], destination)
        return {
            "path": str(destination),
            "fresh": sum(record["fresh"] for record in records),
            "dirty": sum(not record["fresh"] for record in records),
            "units": records,
        }

    def run(self):
        self.host["load_1m_start"] = os.getloadavg()[0]
        self.prepare()
        flow_started = time.monotonic_ns()
        if not self.prime_only:
            self.inject_calibration()
        rounds = []
        round_started = time.monotonic_ns()
        plan, _ = self.json_invoke(
            "start-smoke-0", "task", "start", "--file", str(self.repo / "tasks" / "smoke-0.json"))
        if plan.get("task_id") != "smoke-0":
            raise RuntimeError(f"unexpected initial plan: {plan}")
        for round_id, (task_id, expected_exit) in enumerate(zip(TASKS, EXPECTED_EXITS)):
            if round_id == 0:
                result, _ = self.json_invoke(
                    "run-smoke-0", "task", "run", task_id, "--execute", expected=expected_exit)
            else:
                result, _ = self.json_invoke(
                    f"continue-smoke-{round_id}", "task", "run", task_id,
                    "--from", TASKS[round_id - 1], "--execute", expected=expected_exit)
            inspection, _ = self.json_invoke("show-" + task_id, "task", "show", task_id)
            checks, _ = self.json_invoke("checks-" + task_id, "task", "show", task_id, "--checks")
            write_json(self.evidence / f"round-{round_id}-run.json", result)
            write_json(self.evidence / f"round-{round_id}-inspection.json", inspection)
            write_json(self.evidence / f"round-{round_id}-checks.json", checks)
            findings = None
            if round_id < 2:
                findings, _ = self.json_invoke("findings-" + task_id, "task", "findings", task_id)
                write_json(self.evidence / f"round-{round_id}-findings.json", findings)
            cargo = self.preserve_cargo(round_id)
            kernel = []
            for observation in inspection.get("runtime_observations", []):
                record = observation.get("record", {})
                if record.get("check", {}).get("name") == "kernel":
                    kernel.append({
                        "elapsed_ms": record["spans"][0]["elapsed_ms"],
                        "caches": record.get("caches", []),
                    })
            rounds.append({
                "round": round_id,
                "task_id": task_id,
                "expected_exit": expected_exit,
                "wall_ns": time.monotonic_ns() - round_started,
                "result": result,
                "findings": findings,
                "cargo": cargo,
                "kernel": kernel,
                "check_document": checks,
            })
            round_started = time.monotonic_ns()
            if self.prime_only:
                document = {
                    "schema": "af.perf-dev-flow-seed/1",
                    "fixture": "default-light-smoke",
                    "metrics": {
                        "accepted_wall_ns": time.monotonic_ns() - flow_started,
                        "accepted_wall_ms": (time.monotonic_ns() - flow_started) / 1_000_000,
                        "cache_bytes_after": directory_bytes(self.cache),
                    },
                    "rounds": rounds,
                    "commands": self.commands,
                }
                write_json(self.output, document)
                return document
        task_chain_ns = time.monotonic_ns() - flow_started

        # Final materialization and independent semantic verification are inside the primary
        # interval, so accepted_wall_ms and the supervisor's child lifetime measure the same work.
        materialized, _ = self.json_invoke(
            "materialize-final", "task", "materialize", "smoke-2", "--into", str(self.repo))
        final_target = self.work / "final-target"
        final_env = self.env.copy()
        final_env["CARGO_TARGET_DIR"] = str(final_target)
        oracle = self.work / "oracle"
        (oracle / "src").mkdir(parents=True)
        (oracle / "Cargo.toml").write_text(
            "[package]\nname = \"af-dev-flow-oracle\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n"
            "[dependencies]\nsmoke-app = { path = \"../repo/crates/smoke-app\" }\n"
            "smoke-logic = { path = \"../repo/crates/smoke-logic\" }\n"
            "smoke-support = { path = \"../repo/crates/smoke-support\" }\n", encoding="utf-8")
        oracle_main = (
            "fn main() {\n"
            "    assert_eq!(smoke_support::base(), 2);\n"
            "    assert_eq!(smoke_logic::calculate(), 4);\n"
            "    assert_eq!(smoke_app::feature(), 5);\n"
            "    println!(\"authority semantic oracle passed: 2, 4, 5\");\n"
            "}\n")
        (oracle / "src/main.rs").write_text(oracle_main, encoding="utf-8")
        semantic_started = time.monotonic_ns()
        semantic, _ = self.invoke(
            "final-semantic",
            ["cargo", "run", "--offline", "--manifest-path", str(oracle / "Cargo.toml")],
            env=final_env)
        semantic_ns = time.monotonic_ns() - semantic_started
        accepted_ns = time.monotonic_ns() - flow_started
        app = (self.repo / "crates/smoke-app/src/lib.rs").read_text(encoding="utf-8")
        logic = (self.repo / "crates/smoke-logic/src/lib.rs").read_text(encoding="utf-8")
        support = (self.repo / "crates/smoke-support/src/lib.rs").read_text(encoding="utf-8")
        if "smoke_support::base() + 2" not in logic or "pub fn base() -> i32 {\n    2\n}" not in support:
            raise RuntimeError("final source does not have the declared repaired semantics")
        if "smoke_logic::calculate() + 1" not in app:
            raise RuntimeError("final application feature is absent")

        final_source = {}
        for name in ("smoke-app", "smoke-logic", "smoke-support"):
            path = self.repo / "crates" / name / "src/lib.rs"
            final_source[f"crates/{name}/src/lib.rs"] = read_bounded(path).decode("utf-8")

        invocation_path = self.home / "codex-home" / "invocations.jsonl"
        invocations = [json.loads(line) for line in read_bounded(invocation_path).decode(
            "utf-8").splitlines()] if invocation_path.exists() else []
        role_counts = {}
        for invocation in invocations:
            role = invocation["role"]
            role_counts[role] = role_counts.get(role, 0) + 1
        business_counts = {role: role_counts.get(role, 0)
                           for role in ("implementer", "correctness", "evaluator")}
        expected_business = {"implementer": 3, "correctness": 3, "evaluator": 3}
        if business_counts != expected_business:
            raise RuntimeError(f"unexpected business invocation counts: {business_counts}")
        cache_bytes = directory_bytes(self.cache)
        self.host["load_1m_end"] = os.getloadavg()[0]
        document = {
            "schema": "af.perf-dev-flow/2",
            "fixture": "default-light-smoke",
            "cache_treatment": "primed" if self.primed else "cold",
            "host": self.host,
            "metrics": {
                "accepted_wall_ns": accepted_ns,
                "accepted_wall_ms": accepted_ns / 1_000_000,
                "task_chain_wall_ns": task_chain_ns,
                "semantic_verification_ns": semantic_ns,
                "cache_bytes_after": cache_bytes,
                "final_tree_digest": materialized["snapshot_id"],
                "final_semantics_oracle_sha256": file_sha256(oracle / "src/main.rs"),
                "final_semantics_passed": True,
            },
            "rounds": rounds,
            "final_source": final_source,
            "commands": self.commands,
            "mock": {
                "business_counts": business_counts,
                "admission_count": role_counts.get("admission", 0),
                "invocations": invocations,
            },
            "evidence_root": str(self.evidence),
        }
        write_json(self.output, document)
        return document


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--af", required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--cache-seed")
    parser.add_argument("--prime-only", action="store_true")
    parser.add_argument("--primed", action="store_true")
    parser.add_argument("--telemetry", choices=("off", "spans"), default="off")
    parser.add_argument("--rustup-home", default=os.environ.get(
        "RUSTUP_HOME") or ("/usr/local/rustup" if Path("/usr/local/rustup").exists()
                           else str(Path.home() / ".rustup")))
    parser.add_argument("--cargo-bin", default="/usr/local/cargo/bin"
                        if Path("/usr/local/cargo/bin").exists() else str(Path.home() / ".cargo/bin"))
    args = parser.parse_args()
    document = Driver(args).run()
    print(json.dumps({
        "schema": "af.perf-dev-flow-summary/1",
        "accepted_wall_ms": f"{document['metrics']['accepted_wall_ms']:.6f}",
        "round_wall_ms": [round_["wall_ns"] / 1_000_000 for round_ in document["rounds"]],
        "cargo_fresh": [round_["cargo"]["fresh"] for round_ in document["rounds"]],
        "cargo_dirty": [round_["cargo"]["dirty"] for round_ in document["rounds"]],
        "cache_bytes_after": document["metrics"].get("cache_bytes_after", 0),
        "mock": document.get("mock", {}).get("business_counts", {}),
    }, sort_keys=True))


if __name__ == "__main__":
    main()
