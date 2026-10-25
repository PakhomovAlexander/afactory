"""Stand-ins for telemetry experiments (ADR-0213, ADR-0214): bench documents and an executor.

Every document is built from synthetic pairs or samples and completed by the real bench
script's own `summarize` (or its read sample protocol), so it is exactly what the script would
print for them; nothing here runs AF or measures anything, and no figure is a performance claim.
The stand-in executor answers each execution the way a script run would, writes the tiny
corpus a generator would, and can be told to misbehave per (experiment, side, seed).
"""
import gzip
import json
import os

from perflab import contracts, telemetryexec, telemetrylab

FIXTURES = ("telemetry-bench-concurrent", "telemetry-bench-dense", "telemetry-bench-store")
CASES = ("telemetry-bench-invocation-one 64 KiB", "telemetry-bench-invocation-four 64 KiB")
SHA = {"baseline": "1" * 40, "candidate": "2" * 40, "authority": "3" * 40}
HOST = dict((key, {"value": "stand-in %s" % key}) for key in contracts.HOST_KEYS)
VERSION = "af 0.0.0"


def _run(wall, rss=1000, segments=0):
    return {"wall_ms": wall, "exit_code": 0, "peak_rss_bytes": rss, "segments": segments}


def overhead_doc(seed, pairs, slow=1.0, scale="smoke", extra=None, version=VERSION):
    module = telemetrylab.script_module("scripts/telemetry-overhead.py")
    doc = {"schema": "af/telemetry-overhead@1", "af": {"path": "/x/af", "version": version},
           "machine": {"os": "linux", "arch": "x86_64", "cpus": 4, "load1": 0.5},
           "protocol": {"pairs": pairs, "scale": scale, "seed": seed, "resamples": 10000,
                        "median_bound_pct": 2.0, "upper_bound_pct": 3.0},
           "fixtures": [{"name": name, "command": "task start --execute", "store_events": 10,
                         "pairs": [{"order": ["off", "spans"],
                                    "off": _run(round(1000.0 * slow + index, 3)),
                                    "spans": _run(round(1010.0 * slow + index, 3), 1100, 1)}
                                   for index in range(pairs)]} for name in FIXTURES],
           "drain": {"fixture": "telemetry-bench-dense", "bound_ms": 10.0,
                     "pairs": [{"off_ms": 0.5, "spans_ms": 0.75} for _ in range(pairs)]}}
    doc = module.summarize(doc)
    if extra:
        extra(doc)
    return doc


def invocation_doc(seed, pairs, slow=1.0, version=VERSION):
    module = telemetrylab.script_module("scripts/telemetry-invocation-cost.py")

    def run(wall):
        return dict(_run(wall), drain_ms=None, stdout_bytes=65536)
    doc = {"schema": "af/telemetry-invocation-cost@1",
           "af": {"path": "/x/af", "version": version},
           "machine": {"os": "linux", "arch": "x86_64", "cpus": 4, "load1": None},
           "protocol": {"pairs": pairs, "scale": "smoke", "seed": seed, "resamples": 10000,
                        "wall_bound_pct": 2.0, "rss_slack_bytes": 16 << 20},
           "cases": [{"name": name, "fixture": name.split(" ")[0],
                      "attempts": 1 if "-one " in name else 4, "tool_output_bytes": 65536,
                      "accounting": {"phase": "finished", "chargeable_tokens": "0",
                                     "attempts": 1, "usage": [["settled", None]]},
                      "levels": dict((level, {"pairs": [
                          {"order": ["json", "stream-json"], "json": run(900.0 * slow),
                           "stream-json": run(920.0 * slow)} for _ in range(pairs)]})
                          for level in ("off", "spans"))} for name in CASES]}
    return module.summarize(doc)


def write_corpus(out, runs):
    """A tiny stand-in corpus: one gzip segment per execution and one blob."""
    segments = os.path.join(out, "v1", "segments")
    blobs = os.path.join(out, "v1", "blobs", "ab")
    os.makedirs(segments)
    os.makedirs(blobs)
    for index in range(runs * 2):
        with open(os.path.join(segments, "seg-%05d.jsonl.gz" % index), "wb") as stream:
            stream.write(gzip.compress(b'{"segment": %d}\n' % index, mtime=0))
    module = telemetrylab.script_module("scripts/telemetry-corpus.py")
    for index in range(module.Layout(20261021, runs).blobs):
        with open(os.path.join(blobs, "%06d.gz" % index), "wb") as stream:
            stream.write(gzip.compress(b"blob", mtime=0))
    return out


def latency_doc(runs, slow=1.0, repeat=3, facts=None, name=None, version=VERSION):
    def figure(latency, count):
        samples = [round(latency + offset, 3) for offset in range(count)]
        return {"latency_ms": sorted(samples)[(count - 1) // 2], "peak_rss_bytes": 2000,
                "exit_code": 0, "index_bytes": 4096, "samples_ms": samples}
    measurements = []
    for item, bound in (("fresh_status", 2000), ("unchanged_status", 200), ("show", 200)):
        warm = figure(10.0 * slow, 1 if item == "fresh_status" else repeat)
        measurements.append({"name": item, "command": "af telemetry %s" % item,
                             "bound_ms": bound, "warm": warm, "cold": None,
                             "within_bound": warm["latency_ms"] <= bound})
    module = telemetrylab.script_module("scripts/telemetry-corpus.py")
    expected = module.expected_counts(20261021, runs)
    facts = facts if facts is not None else dict(
        (key, expected.get(key, 1)) for key in telemetrylab.CORPUS_FACTS)
    corpus = {"dir": "/work/corpus",
              "name": name or ("telemetry-corpus-10k" if runs == 10000
                               else "telemetry-corpus-%d" % runs),
              "seed": 20261021, "runs": runs}
    module = telemetrylab.script_module("scripts/telemetry-corpus.py")
    corpus.update(module.expected_counts(corpus["seed"], runs))
    corpus.update(facts)
    return {"schema": "af/telemetry-read-latency@2",
            "af": {"path": "/x/af", "version": version},
            "machine": {"os": "linux", "arch": "x86_64", "cpus": 4, "load1": 1.0},
            "corpus": corpus,
            "cold": {"available": False, "method": None, "reason": "needs root"},
            "measurements": measurements,
            "passed": all(item["within_bound"] for item in measurements)}


def expect(experiment, seed=20261021, pairs=2, scale="smoke", repeat=3, runs=10,
           version=VERSION, facts=None):
    """An expectation as `telemetrylab.expectation` would give it."""
    if experiment == "read_latency":
        corpus = dict(facts if facts is not None else dict(
            (key, telemetrylab.script_module("scripts/telemetry-corpus.py").expected_counts(
                20261021, runs).get(key, 1)) for key in telemetrylab.CORPUS_FACTS),
                      runs=runs, seed=20261021)
        return {"runs": runs, "repeat": repeat, "seed": seed, "version": version,
                "corpus": corpus}
    return {"pairs": pairs, "scale": scale, "seed": seed, "version": version, "corpus": None}


class StandIn(telemetryexec.Executor):
    """Answers each execution as its script would; `behave` maps (experiment, side, seed) or
    (experiment, side) to a dict of document options or a callable(execution) -> Outcome."""

    mode = "trusted_local"
    containment = "process_group"
    python = "python3"
    limitations = telemetryexec.TRUSTED_LOCAL_LIMITATIONS

    def __init__(self, binaries, behave=None, version=VERSION):
        telemetryexec.Executor.__init__(self)
        self.binaries = binaries
        self.behave = behave or {}
        self.version = version
        self.calls = []

    def run(self, execution):
        argv = execution.argv
        self.calls.append(execution)
        if argv[1:] == ["--version"]:
            behave = self.behave.get(("version", self.side(argv)))
            if behave is not None:
                return behave(execution)
            return telemetryexec.Outcome(0, (self.version + "\n").encode(), len(self.version) + 1,
                                         b"", 0, 1000)
        if "generate" in argv:
            out = argv[argv.index("--out") + 1]
            write_corpus(out, int(argv[argv.index("--runs") + 1]))
            return telemetryexec.Outcome(0, b"{}\n", 3, b"generated\n", 10, 1000000)
        side = self.side(argv)
        experiment = {"telemetry-overhead.py": "overhead",
                      "telemetry-invocation-cost.py": "invocation_cost",
                      "telemetry-corpus.py": "read_latency"}[os.path.basename(argv[3])]
        seed = int(argv[argv.index("--seed") + 1]) if "--seed" in argv else None
        pairs = int(argv[argv.index("--pairs") + 1]) if "--pairs" in argv else None
        behave = self.behave.get((experiment, side, seed), self.behave.get((experiment, side)))
        if behave is not None and not isinstance(behave, dict):
            return behave(execution)
        options = dict(behave or {})
        slow = 1.1 if side == "candidate" else 1.0
        if experiment == "overhead":
            doc = overhead_doc(seed, options.pop("pairs", pairs), slow, **options)
        elif experiment == "invocation_cost":
            doc = invocation_doc(seed, pairs, slow, **options)
        else:
            corpus = argv[argv.index("--dir") + 1]
            repeat = int(argv[argv.index("--repeat") + 1])
            facts = telemetrylab.corpus_facts(corpus)
            doc = latency_doc(10, slow, repeat, facts, **options)
        data = (json.dumps(doc, indent=2) + "\n").encode("utf-8")
        code = 3 if doc.get("gate", {}).get("verdict") == "fail" else 0
        return telemetryexec.Outcome(code, data, len(data), b"log line\n", 9, 2000000)

    def side(self, argv):
        return [name for name, path in self.binaries.items() if path in argv][0]


def binaries(test):
    out = {}
    for side in ("baseline", "candidate"):
        path = test.path("%s-af" % side)
        with open(path, "w") as stream:
            stream.write("#!/bin/sh\necho %s\n" % side)
        os.chmod(path, 0o755)
        out[side] = os.path.abspath(path)
    return out


def settings(test, executor, output="result", hosts=None, **values):
    hosts = list(hosts or [HOST, HOST])
    if not os.path.isdir(test.path("lock")):
        os.mkdir(test.path("lock"), 0o700)
    options = dict(profile="manual", trigger="manual",
                   baseline_af=test.binaries["baseline"],
                   candidate_af=test.binaries["candidate"],
                   baseline_sha=SHA["baseline"], candidate_sha=SHA["candidate"],
                   authority_sha=SHA["authority"], pairs=2, scale="smoke", corpus_runs=10,
                   output=test.path(output), result_id="tl_test_" + output,
                   executor=executor, host_identity=lambda: hosts.pop(0),
                   free_bytes=lambda path: 1 << 40, lock_directory=test.path("lock"))
    options.update(values)
    return telemetrylab.Settings(**options)


def rewrite(output, result):
    """Write `result` and its page, summary and manifest again, consistently: a forger's
    bundle, whose every hash agrees."""
    from perflab import bundle
    for name in (telemetrylab.RESULT_JSON, telemetrylab.REPORT_HTML, telemetrylab.SUMMARY_MD,
                 bundle.MANIFEST):
        os.unlink(os.path.join(output, name))
    telemetrylab.write_bundle(output, result)


def rehash(run, data):
    import hashlib
    run["document"]["size"] = len(data)
    run["document"]["sha256"] = "sha256:" + hashlib.sha256(data).hexdigest()
