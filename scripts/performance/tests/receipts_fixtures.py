"""Recipes of the kernel receipts schema fixtures under
fixtures/performance/schema/kernel-receipts (`af.perf-kernel-receipts/3`, ADR-0187).

Constructed documents, never measurements: every envelope and stream identity is computed here
with the kernel's canonical JSON and domains, so the valid fixture is self-consistent and each
invalid fixture breaks exactly one rule. One Task completed and retains its baseline, candidate
and comparison; another was refused before its candidate existed, so its result names, and the
document retains, only its baseline. `python3 -I scripts/performance/tests/receipts_fixtures.py
--write` rewrites the files; the fixture test regenerates them and requires identical bytes.
"""
import copy
import hashlib
import json
import os
import sys

if __name__ == "__main__":
    sys.dont_write_bytecode = True
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from perflab import contracts  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
TARGET = os.path.join(ROOT, "fixtures/performance/schema/kernel-receipts")
NOTE = "a constructed schema fixture, not evidence"
SNAPSHOT = "sha256:" + "16a0eeb0791b6c92451fd284dd9f599e0a7dbe7f6ebea6e2d2d06c7f74aec112"
CANDIDATE_SNAPSHOT = "sha256:" + "5be1f6a4f4fd9d0b9f1b7a8bd42b2dbcf0f0b8c0a1f6b8f2c9e3d6a7b8c9d0e1"
UNRELATED = "sha256:" + "e" * 64


def digest(label):
    return "sha256:" + hashlib.sha256(label.encode("utf-8")).hexdigest()


def attempt(label):
    """A monotonic Attempt ID's spelling: 26 lowercase letters and digits."""
    return digest(label)[len("sha256:"):][:26]


def stream(text):
    data = text.encode("utf-8")
    part = contracts.KERNEL_STREAM_PART_BYTES
    return {"id": contracts.kernel_blob_id(data),
            "sha256": "sha256:" + hashlib.sha256(data).hexdigest(), "bytes": len(data),
            "retained": "whole", "encoding": "utf-8", "head": data[:part].decode("utf-8"),
            "tail": data[part:].decode("utf-8"), "omitted_bytes": 0}


def seal(envelope):
    """Set both of an envelope's identities as the kernel computes them."""
    envelope["content_id"] = contracts.kernel_digest(contracts.KERNEL_CONTENT_DOMAIN,
                                                     envelope["payload"], "payload")
    unsigned = dict((key, item) for key, item in envelope.items() if key != "artifact_id")
    envelope["artifact_id"] = contracts.kernel_digest(contracts.KERNEL_ARTIFACT_DOMAIN,
                                                      unsigned, "envelope")
    return envelope


def measurement(task, side, snapshot, wall_ms):
    streams, runs = [], []
    for repetition in (1, 2):
        value = "%s%d" % (wall_ms, repetition)
        stdout = stream('{"metrics":{"wall_ms":{"unit":"ms","value":"%s"}},'
                        '"schema":"af.measure-report/1"}\n' % value)
        stderr = stream("kernel-measure guard: protected files match (%s %s, repetition %d)\n"
                        % (task, side, repetition))
        streams.append({"repetition": repetition, "stdout": stdout, "stderr": stderr})
        runs.append({"cache": {"bytes": 0, "warm": True}, "elapsed_ms": 1545,
                     "exit_code": 0, "metrics": {"wall_ms": value},
                     "started_unix_ms": 1792548869909 + repetition,
                     "stdout_id": stdout["id"], "stderr_id": stderr["id"]})
    envelope = seal({
        "type": "af/Measurement@1", "artifact_id": None, "content_id": None,
        "producer": {"kind": "attempt", "run_id": "task-%s" % task, "node_id": side,
                     "attempt_id": attempt(task + " " + side)},
        "input_artifacts": [digest(task + " source")], "subject_snapshot_id": snapshot,
        "payload": {"note": NOTE, "outcome": "passed", "runs": runs}})
    return {"artifact_id": envelope["artifact_id"], "type": envelope["type"],
            "snapshot_id": snapshot, "envelope": envelope, "streams": streams}


def comparison(task, baseline, candidate):
    envelope = seal({
        "type": "af/MeasurementComparison@1", "artifact_id": None, "content_id": None,
        "producer": {"kind": "kernel_operation", "run_id": "task-%s" % task,
                     "node_id": "compare", "operation_id": "measurement-comparison@1"},
        "input_artifacts": [baseline["artifact_id"], candidate["artifact_id"]],
        "subject_snapshot_id": candidate["snapshot_id"],
        "payload": {"note": NOTE, "outcome": "improved"}})
    return {"artifact_id": envelope["artifact_id"], "type": envelope["type"],
            "snapshot_id": candidate["snapshot_id"], "envelope": envelope, "streams": []}


def binding(output):
    return {"artifact_ids": [output["artifact_id"]], "artifact_type": output["type"],
            "cardinality": "one", "snapshot_id": output["snapshot_id"]}


def task(task_id, exit_code, outputs, execution, acceptance, conclusion):
    ports = dict((port, binding(output)) for port, output in outputs.items())
    # Ports the receipts never retain are named by the result all the same.
    ports["snapshot"] = {"artifact_ids": [digest(task_id + " snapshot")],
                         "artifact_type": "af/SourceTree@1", "cardinality": "one",
                         "snapshot_id": SNAPSHOT}
    return {"task_id": task_id, "exit_code": exit_code,
            "result": {"task_revision_id": digest(task_id + " revision"),
                       "execution": execution, "acceptance": acceptance,
                       "domain_conclusion": conclusion, "outputs": ports,
                       "evidence": sorted(set(binding["artifact_ids"][0]
                                              for binding in ports.values())),
                       "missing_obligations": [] if acceptance == "satisfied"
                       else ["verified"]},
            "outputs": outputs}


def valid():
    baseline = measurement("improve", "baseline", SNAPSHOT, "1510.5")
    candidate = measurement("improve", "candidate", CANDIDATE_SNAPSHOT, "1.5")
    refused = measurement("tampered", "baseline", SNAPSHOT, "1510.5")
    return {
        "schema": contracts.KERNEL_RECEIPTS_SCHEMA, "harness_digest": "sha256:" + "0" * 64,
        "instance": "canonical_validate_small__package_small_envelope",
        "measure": "perf_canonical_validate_small",
        "objective": "perf_canonical_validate_small_elapsed", "slowdown_ms": 1500,
        "tasks": [
            task("improve", 0, {"baseline": baseline, "candidate": candidate,
                                "comparison": comparison("improve", baseline, candidate)},
                 "completed", "satisfied", "verified"),
            task("tampered", 1, {"baseline": refused}, "exhausted", "unsatisfied",
                 "candidate_refused")]}


def _improve(doc):
    return doc["tasks"][0]


def _restream(doc):
    # A run naming another stream ID, its envelope and every reference to it resealed, so only
    # the stream's own identity is wrong.
    output = _improve(doc)["outputs"]["baseline"]
    output["streams"][0]["stdout"]["id"] = UNRELATED
    output["envelope"]["payload"]["runs"][0]["stdout_id"] = UNRELATED
    seal(output["envelope"])
    output["artifact_id"] = output["envelope"]["artifact_id"]
    _improve(doc)["result"]["outputs"]["baseline"]["artifact_ids"] = [output["artifact_id"]]


def _set(path, value):
    def change(doc):
        target = doc
        for key in path[:-1]:
            target = target[key]
        target[path[-1]] = value
    return change


def _resnapshot(doc):
    # The output and its Task's result agree on a Snapshot that the envelope does not name.
    _improve(doc)["outputs"]["baseline"]["snapshot_id"] = CANDIDATE_SNAPSHOT
    _improve(doc)["result"]["outputs"]["baseline"]["snapshot_id"] = CANDIDATE_SNAPSHOT


def _drop_repetition(doc):
    del _improve(doc)["outputs"]["baseline"]["streams"][1]


def _grow_stream(doc):
    _improve(doc)["outputs"]["baseline"]["streams"][0]["stdout"]["bytes"] += 1


def _rebind(doc):
    """Reseal every retained envelope after a change, in port order, and carry each new
    artifact ID into the comparison's inputs, the output records and every Task result's
    bindings and evidence, so a fixture breaks only the rule it names and its identities are
    the kernel's own."""
    for task in doc["tasks"]:
        renamed = {}
        for port in ("baseline", "candidate", "comparison"):
            output = task["outputs"].get(port)
            if output is None:
                continue
            envelope = output["envelope"]
            envelope["input_artifacts"] = [renamed.get(item, item)
                                           for item in envelope["input_artifacts"]]
            seal(envelope)
            renamed[output["artifact_id"]] = envelope["artifact_id"]
            output["artifact_id"] = envelope["artifact_id"]
        result = task["result"]
        for binding in result["outputs"].values():
            binding["artifact_ids"] = [renamed.get(item, item) for item in binding["artifact_ids"]]
        result["evidence"] = sorted(renamed.get(item, item) for item in result["evidence"])


def _producer(port, name, value):
    def change(doc):
        _improve(doc)["outputs"][port]["envelope"]["producer"][name] = value
        _rebind(doc)
    return change


def _stream_over_part(doc):
    # The root's counterexample: a stream of 65,537 bytes kept whole, split after its first
    # 32 KiB, so its tail holds one byte more than a part may; every identity recomputed.
    part = contracts.KERNEL_STREAM_PART_BYTES
    output = _improve(doc)["outputs"]["baseline"]
    retained = stream("h" * part + "t" * (part + 1))
    output["streams"][0]["stdout"] = retained
    output["envelope"]["payload"]["runs"][0]["stdout_id"] = retained["id"]
    _rebind(doc)


def _satisfied_incomplete(doc):
    _improve(doc)["result"]["execution"] = "incomplete"


def _two_snapshots(doc):
    # Cardinality one with two artifacts, on a port the receipts never retain.
    _improve(doc)["result"]["outputs"]["snapshot"]["artifact_ids"] = sorted(
        [digest("improve snapshot"), digest("another snapshot")])


def _reverse_evidence(doc):
    # The root's counterexample: the improved Task's four distinct evidence IDs in descending
    # order, every envelope and reference unchanged.
    _improve(doc)["result"]["evidence"].reverse()


def _repeat_evidence(doc):
    evidence = _improve(doc)["result"]["evidence"]
    evidence.insert(0, evidence[0])


BASELINE = ("tasks", 0, "outputs", "baseline")
# (file, change, the reader's error, whether the published JSON Schema refuses it too)
INVALID = (
    ("invalid-version-2.json", _set(("schema",), "af.perf-kernel-receipts/2"),
     "expected af.perf-kernel-receipts/3", True),
    ("invalid-null-subject.json",
     _set(BASELINE + ("envelope", "subject_snapshot_id"), None), "expected a string", True),
    ("invalid-envelope-identity.json", _resnapshot,
     "the artifact, type and Snapshot it names", False),
    ("invalid-envelope-names.json",
     _set(BASELINE + ("envelope", "artifact_id"), UNRELATED),
     "the artifact, type and Snapshot it names", False),
    ("invalid-mutated-payload.json",
     _set(BASELINE + ("envelope", "payload", "runs", 0, "metrics", "wall_ms"), "999"),
     "not the kernel's content ID of the payload", False),
    ("invalid-mutated-provenance.json",
     _set(BASELINE + ("envelope", "producer", "attempt_id"), attempt("another")),
     "not the kernel's artifact ID of the envelope", False),
    ("invalid-task-artifact.json",
     _set(("tasks", 0, "result", "outputs", "baseline", "artifact_ids"), [UNRELATED]),
     "the Task result's artifact, type and Snapshot are this output's", False),
    ("invalid-unnamed-output.json",
     lambda doc: doc["tasks"][1]["result"]["outputs"].pop("baseline"),
     "the Task result names no output on this port", False),
    ("invalid-unretained-output.json",
     _set(("tasks", 1, "result", "outputs", "candidate"),
          {"artifact_ids": [UNRELATED], "artifact_type": "af/Measurement@1",
           "cardinality": "one", "snapshot_id": CANDIDATE_SNAPSHOT}),
     "names an output on this port that is not retained", False),
    ("invalid-missing-repetition.json", _drop_repetition, "one per run, in order", False),
    ("invalid-stream-bytes.json", _grow_stream,
     "head, tail and omitted bytes are the stream's bytes", False),
    ("invalid-stream-id.json", _restream, "not the kernel's content ID of the whole stream",
     False),
    ("invalid-retained-claim.json",
     _set(BASELINE + ("streams", 0, "stderr", "retained"), "head_and_tail"),
     "whole exactly when no byte is omitted", False),
    # `ArtifactEnvelope::validate`, `TaskResultV1::validate` and the stream bound, each on an
    # otherwise self-consistent document whose identities are recomputed.
    ("invalid-blank-producer.json", _producer("baseline", "run_id", " "),
     "producer.run_id: blank", False),
    ("invalid-blank-operation.json", _producer("comparison", "operation_id", "\u3000"),
     "producer.operation_id: blank", False),
    ("invalid-blank-conclusion.json",
     _set(("tasks", 1, "result", "domain_conclusion"), "\u2028 "),
     "domain_conclusion: blank", False),
    ("invalid-satisfied-incomplete.json", _satisfied_incomplete,
     "satisfied acceptance requires completed execution", False),
    ("invalid-empty-output.json",
     _set(("tasks", 1, "result", "outputs", "candidate"),
          {"artifact_ids": [], "artifact_type": "af/Measurement@1", "cardinality": "one",
           "snapshot_id": CANDIDATE_SNAPSHOT}),
     "cardinality one names exactly one artifact", False),
    ("invalid-output-cardinality.json", _two_snapshots,
     "cardinality one names exactly one artifact", False),
    ("invalid-stream-part-bytes.json", _stream_over_part,
     "a retained part holds at most 32768 bytes", False),
    # `TaskResultV1`'s two sets, read by `unique_set`: strictly ascending, so a reversed or a
    # repeated entry is refused rather than normalized.
    ("invalid-unsorted-evidence.json", _reverse_evidence,
     "evidence: set entries must be sorted and unique", False),
    ("invalid-unsorted-missing-obligations.json",
     _set(("tasks", 1, "result", "missing_obligations"), ["verified", "measured"]),
     "missing_obligations: set entries must be sorted and unique", False),
    ("invalid-duplicate-evidence.json", _repeat_evidence, "evidence: items must be unique",
     True),
    ("invalid-duplicate-missing-obligations.json",
     _set(("tasks", 1, "result", "missing_obligations"), ["verified", "verified"]),
     "missing_obligations: items must be unique", True),
)


def documents():
    """Every fixture's file name and document, the valid one first."""
    out = [("valid-fixture.json", valid())]
    for name, change, _, _ in INVALID:
        doc = valid()
        change(doc)
        out.append((name, doc))
    return out


def encode(doc):
    return (json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")


def index_entries():
    """The schema index's entries for these fixtures, in path order."""
    entries = [{"path": "kernel-receipts/valid-fixture.json", "contract": "kernel-receipts",
                "valid": True}]
    entries += [{"path": "kernel-receipts/" + name, "contract": "kernel-receipts",
                 "valid": False, "error": error, "shape": shape}
                for name, _, error, shape in INVALID]
    return sorted(entries, key=lambda entry: entry["path"])


def write_all(target=TARGET):
    for name in os.listdir(target):
        os.remove(os.path.join(target, name))
    for name, doc in documents():
        with open(os.path.join(target, name), "wb") as stream:
            stream.write(encode(copy.deepcopy(doc)))


if __name__ == "__main__":
    if sys.argv[1:] != ["--write"]:
        sys.exit("usage: receipts_fixtures.py --write")
    write_all()
