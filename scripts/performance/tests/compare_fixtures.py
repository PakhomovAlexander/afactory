"""Recipes of the SYNTHETIC comparison fixtures whose golden verdicts are committed under
fixtures/performance/compare.

Only `index.json` and each fixture's `verdict.json` are committed (ADR-0179). The run bundles
they judge are generated at test time by `write_all`, deterministically: each golden verdict
names the run bundle's and every shard bundle's manifest identity, which pins every generated
byte, so a generator change that alters one is a changed golden. `python3 -I
scripts/performance/tests/compare_fixtures.py --write` rewrites the goldens; the fixture test
regenerates everything in a temporary directory and requires identical golden bytes.
"""
from fractions import Fraction
import json
import os
import shutil
import sys
import tempfile

if __name__ == "__main__":
    sys.dont_write_bytecode = True
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from perflab import compare, schedule, suite, synthetic  # noqa: E402
from perflab.values import document_bytes  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
TARGET = os.path.join(ROOT, "fixtures/performance/compare")
SUITE = os.path.join(ROOT, "fixtures/performance/synthetic/suite.toml")
FAST = "synthetic_fast"
ADVISORY = "synthetic_advisory"


def _losses(sequence):
    values = [Fraction(item) for item in sequence]

    def value(slot, case):
        if slot["phase"] == "measured" and slot["side"] == schedule.CANDIDATE:
            return Fraction(20) + values[slot["pair"] - 1]
        return Fraction(20)
    return value


def _slower(instance, ms):
    def value(slot, case):
        if case["instance_id"] == instance and slot["side"] == schedule.CANDIDATE:
            return Fraction(20) + Fraction(ms)
        return Fraction(20)
    return value


def _tamper(run):
    path = os.path.join(run, "shards/0/samples.jsonl")
    with open(path, "rb") as stream:
        data = stream.read()
    with open(path, "wb") as stream:
        stream.write(data.replace(b'"value":"20"', b'"value":"19"', 1))


def _swap_first_measured(run):
    """Swap two adjacent records of shard 0 and reseal it: every hash is valid, the order is not."""
    path = os.path.join(run, "shards/0/samples.jsonl")
    with open(path, "rb") as stream:
        lines = stream.read().split(b"\n")
    first = [index for index, line in enumerate(lines) if b'"phase":"measured"' in line][0]
    lines[first], lines[first + 1] = lines[first + 1], lines[first]
    with open(path, "wb") as stream:
        stream.write(b"\n".join(lines))
    synthetic.reseal_shard(os.path.join(run, "shards/0"))


def _advisory_shard(plan):
    return [case["shard"] for case in plan["cases"] if case["instance_id"] == ADVISORY][0]


def _single(loaded, run_id):
    return synthetic.plan(loaded, scope="partial_scope", cases=[FAST], run_id=run_id, seed=3)


def _candidate_oracle(plan):
    order = plan["cases"][0]["measured_orders"][4]
    position = 1 if schedule.side_at(order, 1) == schedule.CANDIDATE else 2
    return {("measured", FAST, 5, position): "oracle_mismatch"}


FIXTURES = {
    "synthetic-pass": {
        "description": "Every case equal on both sides.", "exit_code": 0,
        "plan": lambda s: synthetic.plan(s, run_id="fixture_pass", seed=1),
        "value": lambda plan: synthetic.constant(20)},
    "synthetic-non-regression": {
        "description": "Interval [-2, 1/2]: no material regression, improvement uncertain.",
        "exit_code": 0, "plan": lambda s: _single(s, "fixture_non_regression"),
        "value": lambda plan: _losses([-3, -3, -2, -1, 0, 0, 0, 0, 0, "0.5", 1, 1])},
    "synthetic-inconclusive": {
        "description": "Interval [-1/5, 7/5]: a material regression remains plausible.",
        "exit_code": 4, "plan": lambda s: _single(s, "fixture_inconclusive"),
        "value": lambda plan: _losses([-1, -1, "-0.2", 0, 0, 0, 0, 0, 0, "1.4", 2, 2])},
    "synthetic-regression": {
        "description": "One critical case 10 ms slower on every candidate sample.",
        "exit_code": 3, "plan": lambda s: synthetic.plan(s, run_id="fixture_regression", seed=1),
        "value": lambda plan: _slower("synthetic_process__size_1", 10)},
    "synthetic-candidate-failure": {
        "description": "The candidate fails its oracle once; the baseline is valid.",
        "exit_code": 3, "plan": lambda s: _single(s, "fixture_candidate_failure"),
        "value": lambda plan: synthetic.constant(20), "failures": _candidate_oracle},
    "synthetic-tampered": {
        "description": "A sample byte changed after its manifest was written.",
        "exit_code": 4, "plan": lambda s: _single(s, "fixture_tampered"),
        "value": lambda plan: synthetic.constant(20), "after": _tamper},
    "synthetic-failed-shard": {
        "description": "Shard 0 holds every planned record but its terminal record says failed.",
        "exit_code": 4, "plan": lambda s: synthetic.plan(s, run_id="fixture_failed_shard", seed=1),
        "value": lambda plan: synthetic.constant(20),
        "options": lambda plan: {"shard_status": {0: "failed"}}},
    "synthetic-missing-advisory": {
        "description": "One advisory record was never recorded; its shard says interrupted.",
        "exit_code": 4,
        "plan": lambda s: synthetic.plan(s, run_id="fixture_missing_advisory", seed=1),
        "value": lambda plan: synthetic.constant(20),
        "options": lambda plan: {"omit": [("measured", ADVISORY, 3, 2)],
                                 "shard_status": {_advisory_shard(plan): "interrupted"}}},
    "synthetic-reordered": {
        "description": "Two records of shard 0 swapped in samples.jsonl; manifests rehashed.",
        "exit_code": 4, "plan": lambda s: _single(s, "fixture_reordered"),
        "value": lambda plan: synthetic.constant(20), "after": _swap_first_measured},
    "synthetic-missing-shard": {
        "description": "Shard 1 never uploaded its bundle.", "exit_code": 4,
        "plan": lambda s: synthetic.plan(s, run_id="fixture_missing_shard", seed=1),
        "value": lambda plan: synthetic.constant(20),
        "after": lambda run: shutil.rmtree(os.path.join(run, "shards/1"))},
}


def write_all(target):
    loaded = suite.load(SUITE)
    index = []
    for name in sorted(FIXTURES):
        recipe = FIXTURES[name]
        plan = recipe["plan"](loaded)
        run = os.path.join(target, name, "run")
        failures = recipe["failures"](plan) if "failures" in recipe else None
        options = recipe["options"](plan) if "options" in recipe else {}
        synthetic.write_run(plan, run, recipe["value"](plan), failures=failures, **options)
        if "after" in recipe:
            recipe["after"](run)
        verdict = compare.compare(run)
        assert verdict["aggregate"]["exit_code"] == recipe["exit_code"], name
        with open(os.path.join(target, name, "verdict.json"), "wb") as stream:
            stream.write(document_bytes(verdict))
        index.append({"name": name, "synthetic": True, "description": recipe["description"],
                      "exit_code": recipe["exit_code"]})
    with open(os.path.join(target, "index.json"), "w") as stream:
        stream.write(json.dumps({"schema": "af.perf-compare-fixtures/1",
                                 "note": "SYNTHETIC evidence generated by perflab.synthetic; "
                                         "no number here describes the AF product.",
                                 "fixtures": index}, indent=2) + "\n")


def goldens():
    """The committed part of a fixture tree below `root`: its index and each golden verdict."""
    return ["index.json"] + ["%s/verdict.json" % name for name in sorted(FIXTURES)]


def write_goldens(target):
    """Generate every fixture in a scratch directory and keep only its goldens below `target`."""
    scratch = tempfile.mkdtemp(prefix="af-compare-fixtures-")
    try:
        write_all(scratch)
        for relative in goldens():
            path = os.path.join(target, *relative.split("/"))
            os.makedirs(os.path.dirname(path), exist_ok=True)
            shutil.copyfile(os.path.join(scratch, *relative.split("/")), path)
    finally:
        shutil.rmtree(scratch)


if __name__ == "__main__":
    if sys.argv[1:] != ["--write"]:
        sys.exit("usage: compare_fixtures.py --write")
    if os.path.exists(TARGET):
        shutil.rmtree(TARGET)
    os.makedirs(TARGET)
    write_goldens(TARGET)
