"""The trusted telemetry collection lane (ADR-0200).

A fake transport stands for the GitHub API and its signed artifact storage on another origin; no
network is reached and the token is a literal non-secret marker. The artifacts are real ZIP
containers of result bundles made by stand-in runs (`tests/telemetry_fixtures.py`), measured
under the authority of a disposable trusted repository whose commit holds a copy of this
checkout's authority files. Nothing here measures anything.
"""
import io
import json
import os
import shutil
import time
import zipfile
from datetime import datetime, timezone

from perflab import contracts, gitrefs, telemetrycollect, telemetrylab, tomlfile, transport
from perflab.values import ContractError, sha256_bytes
from tests import telemetry_fixtures as fx
from tests.support import ROOT, Repo, TempDir, read

TOKEN = "marker-token-not-a-secret"
API = "https://api.example.invalid"
STORAGE = "https://storage.example.invalid"
REPOSITORY = "example/af"
WORKFLOW_ID = 77
NOW = datetime(2026, 10, 22, 12, 0, 0, tzinfo=timezone.utc)
SERVICE = os.path.join(ROOT, "scripts", "performance", "service")


def zipped(directory, extra=()):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as container:
        for current, _, files in os.walk(directory):
            for name in sorted(files):
                path = os.path.join(current, name)
                container.writestr(os.path.relpath(path, directory).replace(os.sep, "/"),
                                   read(path))
        for name, data in extra:
            container.writestr(name, data)
    return buffer.getvalue()


class FakeApi(object):
    """Answers like the API (workflow, run and artifact listings, a redirect per download)
    and signed storage; records every request."""

    def __init__(self, runs, artifacts):
        self.runs = runs
        self.artifacts = artifacts
        self.requests = []

    def request(self, url, headers, limit, timeout):
        self.requests.append((url, dict(headers)))
        base = API + "/repos/" + REPOSITORY
        if url == base + "/actions/workflows/performance-telemetry.yml":
            return self.json({"id": WORKFLOW_ID,
                              "path": ".github/workflows/performance-telemetry.yml"})
        if url.startswith(base + "/actions/workflows/%d/runs?" % WORKFLOW_ID):
            return self.json({"total_count": len(self.runs), "workflow_runs": self.runs})
        for run in self.runs:
            if url.startswith(base + "/actions/runs/%d/artifacts?" % run["id"]):
                listed = [dict((key, value) for key, value in item.items() if key != "data")
                          for item in self.artifacts.get(run["id"], [])]
                return self.json({"total_count": len(listed), "artifacts": listed})
        for items in self.artifacts.values():
            for item in items:
                if url == base + "/actions/artifacts/%d/zip" % item["id"]:
                    return transport.Response(302, {"location": "%s/blob/%d?sig=S" % (
                        STORAGE, item["id"])}, b"")
                if url == "%s/blob/%d?sig=S" % (STORAGE, item["id"]):
                    return transport.Response(200, {}, item["data"][:limit + 1])
        return transport.Response(404, {}, b"")

    def json(self, doc):
        return transport.Response(200, {}, json.dumps(doc).encode())


def run_entry(run_id, attempt=1, event="workflow_dispatch", head_sha="2" * 40):
    return {"id": run_id, "run_attempt": attempt, "workflow_id": WORKFLOW_ID,
            "status": "completed", "conclusion": "success", "event": event,
            "head_sha": head_sha, "path": ".github/workflows/performance-telemetry.yml",
            "repository": {"full_name": REPOSITORY}}


def artifact_entry(ident, run_id, name, data, digest=True, expired=False):
    item = {"id": ident, "name": name, "size_in_bytes": len(data), "expired": expired,
            "created_at": "2026-10-21T00:00:00Z", "expires_at": "2026-11-20T00:00:00Z",
            "workflow_run": {"id": run_id}, "data": data}
    if digest:
        item["digest"] = sha256_bytes(data)
    return item


class Collection(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.binaries = fx.binaries(self)
        # The trusted repository: main holds this checkout's authority files.
        self.repo = Repo(self.path("trusted"))
        registry, _ = telemetrylab.load_registry()
        files = dict((relative, read(os.path.join(ROOT, relative)))
                     for relative in telemetrylab.authority_files(registry))
        self.authority = self.repo.commit("authority", files)
        self.repo.git("checkout", "-q", "-b", "side")
        self.untrusted = self.repo.commit("side", {"README.md": "side branch"})
        self.repo.git("checkout", "-q", "main")
        self.config = contracts.validate_telemetry_collector({
            "schema": "af.perf-telemetry-collector/1", "repository": REPOSITORY,
            "workflow": "performance-telemetry.yml", "api_url": API,
            "token_env": "AF_PERF_TELEMETRY_TOKEN", "root": self.path("history"),
            "authority_repository": self.repo.path, "trusted_refs": ["refs/heads/main"],
            "lookback_days": 13, "collection_seconds": 600, "runs_max": 50,
            "artifact_bytes_max": 64 << 20, "bytes_max": 1 << 30, "results_max": 100,
            "retention_days": 365})

    def result(self, run_id, attempt=1, authority=None, output=None, **values):
        output = output or "r%d-%d" % (run_id, attempt)
        telemetrylab.run(fx.settings(
            self, fx.StandIn(self.binaries), output, experiments=["overhead"],
            authority_sha=authority or self.authority,
            result_id=values.pop("result_id", "tl_%d_%d" % (run_id, attempt)), **values))
        return self.path(output)

    def cycle(self, runs, artifacts, now=NOW):
        fake = FakeApi(runs, artifacts)
        api = transport.Collector(fake, REPOSITORY, TOKEN, api_url=API)
        doc = telemetrycollect.Cycle(self.config, api, gitrefs.Git(self.repo.path), time.time,
                                     now).run()
        return doc, fake

    def test_a_result_is_collected_once_verified_under_its_trusted_authority(self):
        data = zipped(self.result(101))
        artifacts = {101: [artifact_entry(1, 101, "perf-telemetry-101-1", data),
                           artifact_entry(2, 101, "perf-telemetry-af-candidate-101-1", b"PK")]}
        doc, fake = self.cycle([run_entry(101)], artifacts)
        self.assertEqual(doc["status"], "ok", doc)
        self.assertEqual([(item["outcome"], item["result_id"]) for item in doc["runs"]],
                         [("collected", "tl_101_1")])
        self.assertEqual(doc["runs"][0]["artifact"]["sha256"], sha256_bytes(data))
        history = contracts.read_file(os.path.join(self.config["root"], "history.json"),
                                      "telemetry-history")
        self.assertEqual([item["result_id"] for item in history["results"]], ["tl_101_1"])
        self.assertEqual(history["results"][0]["authority_sha"], self.authority)
        contracts.read_file(os.path.join(self.config["root"], "collection.json"),
                            "telemetry-collection")
        # The token reached only the API origin; signed storage got no credential.
        for url, headers in fake.requests:
            self.assertEqual("Authorization" in headers, url.startswith(API), url)
        # The kept results are the idempotency record: nothing is downloaded again.
        again, fake = self.cycle([run_entry(101)], artifacts)
        self.assertEqual([item["outcome"] for item in again["runs"]], ["already_collected"])
        self.assertFalse(any("/zip" in url or STORAGE in url for url, _ in fake.requests))
        self.assertFalse([name for name in os.listdir(os.path.join(self.config["root"],
                                                                   "results"))
                          if name.startswith(".")], "no staging is left behind")

    def test_an_authority_outside_the_trusted_refs_is_refused(self):
        # A result measured under a commit only a side branch holds is never verified as
        # trusted, even with this checkout's own authority files.
        data = zipped(self.result(102, authority=self.untrusted))
        doc, _ = self.cycle([run_entry(102)], {102: [artifact_entry(
            3, 102, "perf-telemetry-102-1", data)]})
        self.assertEqual(doc["status"], "failed")
        self.assertEqual(doc["runs"][0]["outcome"], "refused")
        self.assertIn("reachable from none of the trusted refs", doc["runs"][0]["reason"])
        self.assertFalse(os.listdir(os.path.join(self.config["root"], "results")))

    def test_bytes_identity_and_containers_are_checked_before_anything_is_kept(self):
        good = zipped(self.result(103))
        forged = artifact_entry(4, 103, "perf-telemetry-103-1", good)
        forged["digest"] = "sha256:" + "0" * 64
        other_run = zipped(self.result(999, output="other"))
        traversal = zipped(self.result(105), extra=[("../escape.txt", b"x")])
        runs = [run_entry(103), run_entry(104), run_entry(105), run_entry(106),
                run_entry(107, event="schedule", head_sha="9" * 40)]
        artifacts = {
            103: [forged],
            104: [artifact_entry(5, 104, "perf-telemetry-104-1", other_run)],
            105: [artifact_entry(6, 105, "perf-telemetry-105-1", traversal)],
            106: [artifact_entry(7, 106, "perf-telemetry-999-1", good)],
            107: [artifact_entry(8, 107, "perf-telemetry-107-1", zipped(self.result(
                107, trigger="nightly", profile="nightly", rounds=1)))]}
        doc, _ = self.cycle(runs, artifacts)
        reasons = dict((item["run_id"], (item["outcome"], item["reason"]))
                       for item in doc["runs"])
        self.assertIn("not the artifact the API declares", reasons[103][1])
        self.assertIn("holds result tl_999_1, not tl_104_1", reasons[104][1])
        self.assertIn("..", reasons[105][1])
        self.assertIn("is not this run's result", reasons[106][1])
        self.assertIn("is not the run's commit", reasons[107][1])
        self.assertTrue(all(outcome == "refused" for outcome, _ in reasons.values()))
        self.assertFalse(os.listdir(os.path.join(self.config["root"], "results")))
        self.assertFalse(os.path.exists(os.path.join(ROOT, "escape.txt")))

    def test_missing_and_expired_results_are_recorded_and_tried_again(self):
        data = zipped(self.result(108))
        runs = [run_entry(108), run_entry(109)]
        doc, _ = self.cycle(runs, {108: [artifact_entry(9, 108, "perf-telemetry-108-1", data,
                                                        expired=True)]})
        self.assertEqual([item["outcome"] for item in doc["runs"]], ["expired", "no_result"])
        self.assertEqual(doc["status"], "ok")

    def test_the_root_s_retention_prunes_whole_results_under_its_lock(self):
        data = zipped(self.result(110))
        self.cycle([run_entry(110)], {110: [artifact_entry(10, 110, "perf-telemetry-110-1",
                                                           data)]})
        with telemetrylab.HistoryRoot(self.config["root"]):
            with self.assertRaises(ContractError):
                self.cycle([], {})
        self.config["retention_days"] = 1
        doc, _ = self.cycle([], {}, now=datetime(2100, 1, 1, tzinfo=timezone.utc))
        self.assertEqual(doc["results"], 1)
        future = telemetrycollect.Cycle(self.config, transport.Collector(
            FakeApi([], {}), REPOSITORY, TOKEN, api_url=API), gitrefs.Git(self.repo.path),
            lambda: 4102444800.0, NOW).run()
        self.assertEqual([item["result_id"] for item in future["pruned"]], ["tl_110_1"])
        self.assertEqual(future["results"], 0)

    def test_the_example_configuration_and_units_are_the_lane_s_own(self):
        path = os.path.join(SERVICE, "telemetry-collector.example.toml")
        doc = telemetrycollect.load_config(path)
        self.assertEqual(doc["workflow"], "performance-telemetry.yml")
        self.assertNotEqual(doc["token_env"], transport.TOKEN_ENV)
        unit = read(os.path.join(SERVICE, "af-perf-telemetry-collect.service")).decode()
        self.assertIn("cli.py telemetry-collect --config /etc/af-perf-telemetry/collector.toml",
                      unit)
        self.assertNotIn("collect.py --root", unit, "the archive lane is left as it is")
        self.assertIn("OnCalendar=hourly", read(os.path.join(
            SERVICE, "af-perf-telemetry-collect.timer")).decode())
        raw = tomlfile.loads(read(path), path)
        for mutate in (lambda d: d.update(workflow="performance.yml"),
                       lambda d: d.update(root=d["authority_repository"] + "/history"),
                       lambda d: d.update(artifact_bytes_max=d["bytes_max"] + 1),
                       lambda d: d.update(trusted_refs=["main"])):
            broken = dict(raw)
            mutate(broken)
            with self.assertRaises(ContractError):
                contracts.validate_telemetry_collector(broken)
        shutil.rmtree(self.config["root"], ignore_errors=True)
