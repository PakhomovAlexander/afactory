"""Evidence-derived oracle and exploratory metrics for the default-light development flow.

The validator inspects retained public Task/check/Finding documents, package-level Cargo artifacts,
mock invocations and final source. It never treats a driver's aggregate summary as proof.
"""

import argparse
import json
import math
import os
from pathlib import Path
import re
import statistics

from . import admission


TASKS = ("smoke-0", "smoke-1", "smoke-2")
EXITS = (3, 3, 0)
OUTCOMES = ("changes_requested", "changes_requested", "verified")
REVIEW_CONCLUSIONS = ("changes_requested", "changes_requested", "pass")
BUSINESS = {"implementer": 3, "correctness": 3, "evaluator": 3}
COLD = ((0, 6), (2, 4), (0, 6))
PRIMED = ((6, 0), (2, 4), (0, 6))
CRATES = ("smoke-app", "smoke-logic", "smoke-support")
SOURCE_PATHS = tuple(f"crates/{name}/src/lib.rs" for name in CRATES)
COMMAND_LABELS = (
    "start-smoke-0", "run-smoke-0", "show-smoke-0", "checks-smoke-0", "findings-smoke-0",
    "continue-smoke-1", "show-smoke-1", "checks-smoke-1", "findings-smoke-1",
    "continue-smoke-2", "show-smoke-2", "checks-smoke-2", "materialize-final", "final-semantic",
)
MAX_EVIDENCE_BYTES = 1048576
CONTAINED_EVIDENCE_ROOT = "/perf/scratch/work/evidence"


def _transferred_json(data):
    # Flow metrics include finite JSON fractions. The laboratory's integer-only control
    # parser cannot read them; still refuse duplicate fields and non-JSON numeric constants.
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f"duplicate evidence field: {key}")
            result[key] = value
        return result

    def number(text):
        value = float(text)
        require(math.isfinite(value), "non-finite evidence number")
        return value

    return json.loads(data, object_pairs_hook=unique, parse_float=number, parse_constant=number)


def transfer_paths():
    """The reviewed evidence closure, relative to this sample's owned scratch."""
    names = [f"command-{index:02d}-{label}.{stream}"
             for index, label in enumerate(COMMAND_LABELS) for stream in ("stdout", "stderr")]
    names += [f"round-{index}-{kind}.{extension}"
              for index in range(3)
              for kind, extension in (("run", "json"), ("inspection", "json"),
                                      ("checks", "json"), ("cargo", "jsonl"))]
    names += [f"round-{index}-findings.json" for index in range(2)]
    return sorted(["flow.json"] + ["work/evidence/" + name for name in names])


def validate_transferred(files, treatment=None):
    """Judge the exact side-written flow and supporting bytes from one supervisor reply.

    The flow's original evidence_root is checked, never followed on the host or rewritten.
    This uses the same strict oracle as a trusted-local sample, with bounded byte-backed
    evidence reads in place of its filesystem reads. When supplied, treatment binds the
    evidence to the planned instance; omission permits generic document classification.
    """
    require(isinstance(files, dict) and sorted(files) == transfer_paths(),
            "missing or unexpected development-flow evidence transfer")
    require(all(isinstance(data, bytes) and len(data) <= MAX_EVIDENCE_BYTES
                for data in files.values()), "invalid or oversized development-flow evidence")
    require(sum(map(len, files.values())) <= 16 * MAX_EVIDENCE_BYTES,
            "development-flow evidence exceeds the aggregate byte bound")
    try:
        document = _transferred_json(files["flow.json"])
        require(isinstance(document, dict), "flow document is not an object")
        require(document.get("evidence_root") == CONTAINED_EVIDENCE_ROOT,
                "flow evidence root differs from the declared container scratch")
        return validate(document, treatment=treatment, evidence=files)
    except (KeyError, TypeError, AttributeError, UnicodeError, IndexError, StopIteration,
            RecursionError, OverflowError) as error:
        raise ValueError(f"malformed development-flow evidence: {error}") from error


def require(condition, message):
    if not condition:
        raise ValueError(message)


def _regular_file(path, limit=MAX_EVIDENCE_BYTES):
    stat = os.lstat(path)
    require(os.path.isfile(path) and not os.path.islink(path), f"not a regular evidence file: {path}")
    require(stat.st_size <= limit, f"evidence exceeds {limit} bytes: {path}")


def read_document(path):
    """Read one bounded JSON document through held no-follow descriptors."""
    root, name = os.path.split(path)
    descriptor, info = admission.open_confined(root or ".", name, "development-flow evidence")
    try:
        require(info.st_size <= MAX_EVIDENCE_BYTES, "flow document exceeds 1 MiB")
        chunks = []
        admission.read_bounded(descriptor, info.st_size, None, path, "reading development-flow evidence",
                               chunks.append)
        document = json.loads(b"".join(chunks).decode("utf-8"))
        document["_evidence_base"] = os.path.dirname(path)
        return document
    finally:
        os.close(descriptor)


def _json(path):
    return read_document(path)


def _jsonl(path):
    descriptor, info = admission.open_confined(
        os.path.dirname(path), os.path.basename(path), "development-flow Cargo evidence")
    try:
        require(info.st_size <= MAX_EVIDENCE_BYTES, "Cargo evidence exceeds 1 MiB")
        chunks = []
        admission.read_bounded(descriptor, info.st_size, None, path, "reading Cargo evidence",
                               chunks.append)
        return [json.loads(line) for line in b"".join(chunks).decode("utf-8").splitlines()]
    finally:
        os.close(descriptor)


def _freshness(rounds, treatment):
    expected = COLD if treatment == "cold" else PRIMED
    for index, (fresh_count, dirty_count) in enumerate(expected):
        cargo = rounds[index].get("cargo", {})
        require(cargo.get("fresh") == fresh_count and cargo.get("dirty") == dirty_count,
                f"round {index}: freshness mismatch {cargo.get('fresh')}/{cargo.get('dirty')}, "
                f"expected {fresh_count}/{dirty_count}")
        units = cargo.get("units", [])
        require(len(units) == 6, f"round {index}: expected six Cargo units, got {len(units)}")
        for crate in CRATES:
            crate_units = [unit for unit in units if unit.get("package") == crate]
            require(len(crate_units) == 2,
                    f"round {index}: expected two {crate} units, got {len(crate_units)}")
        if treatment == "primed" and index == 0:
            expected_dirty = set()
        elif index == 1:
            expected_dirty = {"smoke-logic", "smoke-app"}
        else:
            expected_dirty = set(CRATES)
        for crate in CRATES:
            crate_units = [unit for unit in units if unit.get("package") == crate]
            for unit in crate_units:
                require(unit.get("fresh") == (crate not in expected_dirty),
                        f"round {index}: wrong freshness for {crate}: {unit}")
        require(sum(bool(unit.get("fresh")) for unit in units) == fresh_count,
                f"round {index}: unit identities disagree with fresh count")
        require(sum(not bool(unit.get("fresh")) for unit in units) == dirty_count,
                f"round {index}: unit identities disagree with dirty count")


def _final_source(document):
    source = document.get("final_source", {})
    require(set(source) == set(SOURCE_PATHS), "final source does not cover exactly the three crates")
    for path in SOURCE_PATHS:
        require(isinstance(source[path], str) and len(source[path]) <= 16384,
                f"invalid or oversized final source: {path}")
    support = re.sub(r"/\*.*?\*/|//[^\n]*", "", source[SOURCE_PATHS[2]], flags=re.DOTALL)
    logic = re.sub(r"/\*.*?\*/|//[^\n]*", "", source[SOURCE_PATHS[1]], flags=re.DOTALL)
    app = re.sub(r"/\*.*?\*/|//[^\n]*", "", source[SOURCE_PATHS[0]], flags=re.DOTALL)
    require(re.search(r"pub fn base\(\) -> i32 \{\s*2\s*\}", support), "support base is not two")
    require(re.search(r"pub fn calculate\(\) -> i32 \{\s*smoke_support::base\(\) \+ 2\s*\}", logic),
            "logic does not add two to support")
    require(re.search(r"pub fn feature\(\) -> i32 \{\s*smoke_logic::calculate\(\) \+ 1\s*\}", app),
            "application feature does not add one")


def _evidence(document, transferred=None):
    root = Path(document.get("evidence_root", ""))
    if transferred is None and not root.is_absolute():
        root = Path(document.get("_evidence_base", ".")) / root
    if transferred is None:
        require(root.is_dir() and not root.is_symlink(), "missing evidence root")

    def raw(path):
        name = "work/evidence/" + path.relative_to(CONTAINED_EVIDENCE_ROOT).as_posix()
        require(name in transferred, f"missing transferred evidence: {name}")
        return transferred[name]

    def read_json(path):
        return _json(path) if transferred is None else _transferred_json(raw(path))

    def read_jsonl(path):
        return _jsonl(path) if transferred is None else [_transferred_json(line)
                                                       for line in raw(path).splitlines()]

    commands = document.get("commands", [])
    require(tuple(command.get("label") for command in commands) == COMMAND_LABELS,
            "wrong AF command sequence")
    for index, command in enumerate(commands):
        expected_exit = EXITS[int(command["label"][-1])] if command["label"] in (
            "run-smoke-0", "continue-smoke-1", "continue-smoke-2") else 0
        require(command.get("index") == index and command.get("exit_code") == expected_exit,
                f"command {index}: wrong index or exit code")
        require(command.get("output_overflow") is False,
                f"nested output overflow in {command.get('label')}")
        for stream in ("stdout", "stderr"):
            path = root / f"command-{command['index']:02d}-{command['label']}.{stream}"
            if transferred is None:
                _regular_file(path)
            else:
                raw(path)
    for index in range(3):
        round_ = document["rounds"][index]
        run = read_json(root / f"round-{index}-run.json")
        inspection = read_json(root / f"round-{index}-inspection.json")
        checks = read_json(root / f"round-{index}-checks.json")
        public_run = {key: value for key, value in run.items() if key != "_evidence_base"}
        public_inspection = {key: value for key, value in inspection.items() if key != "_evidence_base"}
        require(public_run == round_["result"] and public_inspection == round_["result"],
                f"round {index}: retained Task documents disagree with the driver")
        public_checks = {key: value for key, value in checks.items() if key != "_evidence_base"}
        require(public_checks == round_["check_document"],
                f"round {index}: retained check document disagrees with the driver")
        artifacts = [message for message in read_jsonl(root / f"round-{index}-cargo.jsonl")
                     if message.get("reason") == "compiler-artifact"]
        require(len(artifacts) == len(round_["cargo"]["units"]),
                f"round {index}: Cargo unit count differs from the retained stream")
        for unit, artifact in zip(round_["cargo"]["units"], artifacts):
            package = artifact["package_id"].split()[0].rsplit("/", 1)[-1].split("#", 1)[0]
            require(unit["package"] == package and unit["target"] == artifact["target"]["name"]
                    and unit["test_profile"] == artifact["profile"]["test"]
                    and unit["fresh"] == artifact["fresh"],
                    f"round {index}: Cargo unit differs from the retained stream")
        if index < 2:
            findings = read_json(root / f"round-{index}-findings.json")
            public_findings = {key: value for key, value in findings.items() if key != "_evidence_base"}
            require(public_findings == round_["findings"],
                    f"round {index}: retained Findings document disagrees with the driver")


def validate(document, treatment=None, evidence=None):
    """Validate one complete retained flow and return its cache treatment."""
    require(document.get("schema") == "af.perf-dev-flow/2", "not an af.perf-dev-flow/2 document")
    rounds = document.get("rounds")
    require(isinstance(rounds, list) and len(rounds) == 3, "expected exactly three rounds")
    actual_treatment = document.get("cache_treatment")
    require(actual_treatment in ("cold", "primed"), "unknown cache treatment")
    if treatment is not None:
        require(actual_treatment == treatment, f"expected {treatment}, found {actual_treatment}")
    for index, (task_id, exit_code, outcome, review_conclusion) in enumerate(
            zip(TASKS, EXITS, OUTCOMES, REVIEW_CONCLUSIONS)):
        round_ = rounds[index]
        require(round_.get("round") == index, f"round {index}: wrong index")
        require(round_.get("task_id") == task_id, f"round {index}: wrong Task")
        require(round_.get("expected_exit") == exit_code, f"round {index}: wrong expected exit")
        require(isinstance(round_.get("wall_ns"), int) and round_["wall_ns"] >= 0,
                f"round {index}: invalid wall time")
        result = round_.get("result", {})
        require(result.get("schema") == "af/task-inspection@23",
                f"round {index}: unsupported inspection schema")
        require(result.get("state", {}).get("phase") == "finished",
                f"round {index}: Task is not finished")
        require(result["state"].get("outcome") == outcome,
                f"round {index}: wrong Task outcome {result['state'].get('outcome')}")
        require(result.get("result", {}).get("domain_conclusion") == outcome,
                f"round {index}: wrong domain conclusion")
        reviews = result.get("review_rounds", [])
        require(len(reviews) == 1 and reviews[0].get("conclusion") == review_conclusion,
                f"round {index}: wrong review conclusion")
        if index:
            continuation = result.get("continues", {})
            require(continuation.get("task_id") == TASKS[index - 1],
                    f"round {index}: wrong continuation source")
            require(continuation.get("round") == 1 and continuation.get("findings") == 2,
                    f"round {index}: continuation does not carry reviewer and evaluator findings")
        checks = round_.get("check_document", {})
        require(checks.get("schema") == "af/task-checks@4", f"round {index}: unsupported check view")
        require(checks.get("gate", {}).get("status") == "passed",
                f"round {index}: gate did not pass")
        receipts = checks.get("receipt", {}).get("checks", [])
        require({receipt.get("name") for receipt in receipts} == {"kernel", "markdownlint"},
                f"round {index}: wrong check set")
        for receipt in receipts:
            require(receipt.get("status") == "passed" and receipt.get("exit_code") == 0,
                    f"round {index}: check {receipt.get('name')} did not run and pass")
        kernel = round_.get("kernel", [])
        require(len(kernel) == 1 and kernel[0].get("elapsed_ms", -1) >= 0,
                f"round {index}: missing real kernel observation")
        report_nodes = result.get("run_reports", [])
        admissions = [node for report in report_nodes for node in report.get("report", {}).get("nodes", [])
                      if node.get("node") in ("root.providers.admit0", "root.providers.admit1")]
        require(len(admissions) == 2 and all(node.get("outcome", {}).get("kind") == "completed"
               for node in admissions),
                f"round {index}: public Provider admissions are incomplete")
        if index < 2:
            findings = round_.get("findings", {})
            require(findings.get("schema") == "af/task-findings@3",
                    f"round {index}: unsupported Findings document")
            require(findings.get("round", {}).get("conclusion") == "changes_requested",
                    f"round {index}: expected changes_requested Findings")
            require(findings["round"].get("counts", {}).get("major") == 1,
                    f"round {index}: expected one major Finding")
            require(findings.get("evaluation", {}).get("outcome") == "failed",
                    f"round {index}: failed evaluator is absent")
    _freshness(rounds, actual_treatment)
    mock = document.get("mock", {})
    require(mock.get("business_counts") == BUSINESS, "wrong business-role counts")
    admissions = [entry for entry in mock.get("invocations", [])
                  if entry.get("role") == "admission"]
    require(len(admissions) == 6 and mock.get("admission_count") == 6,
            "expected exactly six Provider admissions")
    for model in ("gpt-6.2-sol", "gpt-6.1-terra"):
        selected = [entry for entry in admissions if entry.get("model") == model]
        require(len(selected) == 3, f"expected three {model} admissions")
        for entry in selected:
            require(entry.get("binding") == f"codex-main:{model}:high"
                    and entry.get("effort") == "high",
                    f"wrong {model} admission binding")
    roles = [entry.get("role") for entry in mock.get("invocations", [])
             if entry.get("role") != "admission"]
    require(roles == ["implementer", "correctness", "evaluator"] * 3,
            f"wrong business invocation sequence: {roles}")
    findings = [entry.get("finding_count") for entry in mock["invocations"]
                if entry.get("role") == "correctness"]
    require(findings == [1, 1, 0], f"wrong correctness Finding sequence: {findings}")
    evaluations = [entry.get("outcome") for entry in mock["invocations"]
                   if entry.get("role") == "evaluator"]
    require(evaluations == ["failed", "failed", "passed"],
            f"wrong evaluator sequence: {evaluations}")
    host = document.get("host", {})
    require(host.get("cpu_count", 0) > 0, "missing host CPU count")
    for key in ("af_binary_sha256", "fixture_sha256"):
        require(str(host.get(key, "")).startswith("sha256:"), f"missing {key}")
    for key in ("load_1m_start", "load_1m_end"):
        require(isinstance(host.get(key), (int, float)) and host[key] >= 0,
                f"missing {key}")
    metrics = document.get("metrics", {})
    require(metrics.get("accepted_wall_ns", -1) >= 0, "missing accepted wall time")
    require(metrics.get("task_chain_wall_ns", -1) >= 0, "missing Task-chain wall time")
    require(metrics.get("semantic_verification_ns", -1) >= 0, "missing semantic verification time")
    require(metrics.get("final_semantics_passed") is True,
            "final semantic command did not pass")
    require(str(metrics.get("final_tree_digest", "")).startswith("sha256:"),
            "missing final Tree Digest")
    require(str(metrics.get("final_semantics_oracle_sha256", "")).startswith("sha256:"),
            "missing authority semantic oracle digest")
    semantic = next(command for command in document["commands"] if command["label"] == "final-semantic")
    require(semantic.get("exit_code") == 0, "authority semantic oracle failed")
    _final_source(document)
    _evidence(document, evidence)
    return actual_treatment


def _median(values):
    return statistics.median(values) if values else None


def summarize(documents):
    """Compute an explicitly exploratory median report from validated flow documents."""
    grouped = {}
    for document in documents:
        treatment = validate(document)
        grouped.setdefault(treatment, []).append(document)
    result = {"schema": "af.perf-dev-flow-metrics/1", "calibrated": False,
              "treatments": {}}
    for treatment, flows in sorted(grouped.items()):
        rounds = []
        for index in range(3):
            rounds.append({
                "round": index,
                "wall_ms_median": _median([flow["rounds"][index]["wall_ns"] / 1_000_000
                                           for flow in flows]),
                "kernel_ms_median": _median([
                    flow["rounds"][index]["kernel"][0]["elapsed_ms"] for flow in flows]),
                "cargo_fresh": [flow["rounds"][index]["cargo"]["fresh"] for flow in flows],
                "cargo_dirty": [flow["rounds"][index]["cargo"]["dirty"] for flow in flows],
            })
        result["treatments"][treatment] = {
            "sample_count": len(flows),
            "accepted_wall_ms": [flow["metrics"]["accepted_wall_ms"] for flow in flows],
            "accepted_wall_ms_median": _median([
                flow["metrics"]["accepted_wall_ms"] for flow in flows]),
            "rounds": rounds,
        }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("flows", nargs="+", type=Path)
    args = parser.parse_args()
    documents = [json.loads(path.read_text(encoding="utf-8")) for path in args.flows]
    print(json.dumps(summarize(documents), indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
