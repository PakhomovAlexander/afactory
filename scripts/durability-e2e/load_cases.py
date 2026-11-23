"""Native case entry points and workload fingerprinting for manual load trials."""
from dataclasses import dataclass
from hashlib import file_digest, sha256
import json
from pathlib import Path
import stat


@dataclass(frozen=True)
class Case:
    script: str
    summary: str
    args: tuple[str, ...]
    expectation: str
    seconds: int
    checks: str = ""
    namespace: bool = False

    def arguments(self, binary, source):
        return [binary, *self.args] if self.script == "budget" else [binary, source, *self.args]


def recovery(result, fault, expectation="recover"):
    return Case("recovery", "recovery-summary.json", (result, "--fault", fault),
                expectation, 150, "requirements", fault == "disk-full")


def budget(fault):
    return Case("budget", "budget-summary.json", ("--fault", fault), "policy", 260, "requirements")


CASES = {
    "clean": Case("smoke", "summary.json", ("green",), "control", 110),
    "gate-red": Case("smoke", "summary.json", ("red",), "policy", 110),
    "restart-green": recovery("green", "lost-reply"),
    "restart-red": recovery("red", "lost-reply"),
    "expired-auth": recovery("green", "expired-auth"),
    "wrong-revision": recovery("green", "wrong-revision"),
    "journal-enospc": recovery("green", "disk-full"),
    "budget": budget("attempt-reservation-overrun"),
    "ambiguous-charge": budget("lost-usage"),
    "hostile-output": budget("hostile-output"),
    "malformed-usage": budget("malformed-usage"),
    "writer-loss": Case("writer_loss", "writer-summary.json", (), "recover", 155, "checks"),
    "child-cancel": Case("child_cancel", "cancel-summary.json", (), "policy", 130, "invariants"),
    "admission": Case("admission", "admission-summary.json", (), "policy", 100, "checks"),
    "pause": Case("admission", "admission-summary.json", ("--pause",), "policy", 110, "checks"),
    "artifact-enospc": Case("artifact_fault", "artifact-summary.json", (), "recover", 120,
                            "evidence", True),
    "gate-repair": Case("gate_repair", "repair-summary.json", (), "recover", 250, "checks"),
    "independent-writer": Case("worker_capabilities", "", (), "recover", 20),
}

FIXTURES = (
    "fixtures/task-runtime/pagination",
    "fixtures/task-runtime/continue",
    "fixtures/scenarios/impl/generate.py",
    "scripts/scenario-impl.py",
    "fixtures/native-catalog-contracts/.af/task-packages/kernel",
    "schemas/implementation-test-evidence-v1.json",
    "schemas/implementation-evaluation-input-v1.json",
)
SUITE_FILES = (
    "load_cases.py", "load_worker.py", "worker_failover.py", "check_trace.py", "compare.py", "native_smoke.py", "native_recovery.py",
    "native_budget.py", "native_child_cancel.py", "native_writer_loss.py", "native_admission.py",
    "native_artifact_fault.py", "native_gate_repair.py", "native_worker_capabilities.py",
    "check_adapter.py", "check_service.py", "codex_fixture.py", "mock_provider.py",
    "manifests/worker-failover.json",
)
MARKER = "AF_DURABILITY_LOAD "
DIGEST_PATTERN = r"[0-9a-f]{64}"


def digest(value):
    return sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def file_sha(path):
    with path.open("rb") as source:
        return file_digest(source, "sha256").hexdigest()


def files_identity(root, names):
    """Include hidden fixture files and executable bits. Refuse links and special files."""
    entries = []

    def visit(path):
        metadata = path.lstat()
        mode = metadata.st_mode
        relative = path.relative_to(root).as_posix()
        if path.resolve() != path:
            raise ValueError(f"fixture path traverses a symlink: {path}")
        if stat.S_ISDIR(mode):
            entries.append((relative, "directory"))
            for child in sorted(path.iterdir()):
                visit(child)
        elif stat.S_ISREG(mode) and metadata.st_nlink == 1:
            entries.append((relative, mode & 0o111, file_sha(path)))
        else:
            raise ValueError(f"expected a regular fixture file or directory: {path}")

    for name in names:
        visit(root / name)
    return digest(entries)


def load_profiles(path):
    manifest = json.loads(Path(path).read_text())
    if set(manifest) != {"schema", "profiles"} or manifest["schema"] != "af-durability/load-cases@1":
        raise ValueError("expected af-durability/load-cases@1")
    for name, cases in manifest["profiles"].items():
        if (not isinstance(cases, list) or not cases or any(case not in CASES for case in cases)
                or len(set(cases)) != len(cases)):
            raise ValueError(f"{name}: expected an ordered list of distinct native case IDs")
    return manifest["profiles"]
