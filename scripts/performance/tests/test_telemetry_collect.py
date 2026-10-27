"""The trusted telemetry collection lane (ADR-0215).

A fake transport stands for the GitHub API and its signed artifact storage on another origin; no
network is reached and the token is a literal non-secret marker. The artifacts are real ZIP
containers of result bundles made by stand-in runs (`tests/telemetry_fixtures.py`), measured
under the authority of a disposable trusted repository whose commit holds a copy of this
checkout's authority files. Nothing here measures anything.
"""
import io
import copy
import json
import os
import shutil
import time
import zipfile
from datetime import datetime, timezone
from unittest import mock
from urllib.parse import parse_qs, urlsplit

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


def binding_zip(binding):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as container:
        container.writestr("binding.json", contracts.write_bytes(binding, "telemetry-binding"))
    return buffer.getvalue()


def plan_fixtures(test, runs, artifacts, add_plans=True):
    runs, artifacts = copy.deepcopy(runs), copy.deepcopy(artifacts)
    for run in runs:
        if run["head_sha"] == "2" * 40:
            run["head_sha"] = test.authority
        if not add_plans:
            continue
        for item in list(artifacts.get(run["id"], [])):
            match = telemetrycollect.ARTIFACT.fullmatch(item["name"])
            if match is None:
                continue
            attempt = int(match.group(2))
            planned = test.planned.get((run["id"], attempt))
            if planned is None:
                continue
            binding = dict(planned, schema=contracts.TELEMETRY_BINDING_SCHEMA,
                           repository=REPOSITORY, run_id=run["id"], attempt=attempt,
                           event=run["event"], event_sha=run["head_sha"],
                           run_head_sha=run["head_sha"])
            data = binding_zip(binding)
            artifacts[run["id"]].append(artifact_entry(
                100000 + item["id"], run["id"],
                "telemetry-plan-%d-%d" % (run["id"], attempt), data))
    return runs, artifacts


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
            return self.json(self.page(url, "workflow_runs", sorted(
                self.runs, key=lambda run: run["id"], reverse=True)))
        for run in self.runs:
            if url.startswith(base + "/actions/runs/%d/artifacts?" % run["id"]):
                listed = [dict((key, value) for key, value in item.items() if key != "data")
                          for item in self.artifacts.get(run["id"], [])]
                return self.json(self.page(url, "artifacts", listed))
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

    def page(self, url, key, rows):
        query = parse_qs(urlsplit(url).query)
        size, page = int(query["per_page"][0]), int(query["page"][0])
        return {"total_count": len(rows), key: rows[(page - 1) * size:page * size]}


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
        self.planned = {}
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
        settings = fx.settings(
            self, fx.StandIn(self.binaries), output, experiments=["overhead"],
            authority_sha=authority or self.authority,
            result_id=values.pop("result_id", "tl_%d_%d" % (run_id, attempt)), **values)
        # Independent frozen planning inputs, not copied from the result artifact.
        self.planned[(run_id, attempt)] = {
            "candidate_sha": settings.candidate_sha, "baseline_sha": settings.baseline_sha,
            "control_sha": settings.authority_sha, "profile": settings.profile}
        telemetrylab.run(settings)
        return self.path(output)

    def cycle(self, runs, artifacts, now=NOW, add_plans=True):
        runs, artifacts = plan_fixtures(self, runs, artifacts, add_plans)
        fake = FakeApi(runs, artifacts)
        api = transport.Collector(fake, REPOSITORY, TOKEN, api_url=API)
        doc = telemetrycollect.Cycle(self.config, api, gitrefs.Git(self.repo.path), time.time,
                                     now).run()
        return doc, fake

    def test_repeated_production_cycles_reach_all_350_missing_runs(self):
        self.config["runs_max"] = 200
        runs = [run_entry(ident) for ident in range(1, 351)]
        selected = []
        for _ in range(3):
            doc, fake = self.cycle(runs, {})
            ids = [item["run_id"] for item in doc["runs"]]
            selected.append(ids)
            self.assertEqual(len(ids), 200)
            self.assertEqual(doc["status"], "failed", "a capped cycle is incomplete")
            self.assertEqual({item["outcome"] for item in doc["runs"]}, {"no_result"})
            pages = [int(parse_qs(urlsplit(url).query)["page"][0])
                     for url, _ in fake.requests if "/runs?" in url]
            self.assertEqual(pages, [1, 2, 3, 4])
        self.assertEqual(selected[0], list(range(1, 201)))
        self.assertEqual(selected[1], list(range(201, 351)) + list(range(1, 51)))
        self.assertEqual(set().union(*map(set, selected)), set(range(1, 351)))

    def test_rotation_keeps_old_results_retries_and_higher_attempts_eligible(self):
        self.config["runs_max"] = 137  # Deliberately not a page multiple.
        runs = [run_entry(ident) for ident in range(1, 351)]
        data = {ident: zipped(self.result(ident)) for ident in (1, 2, 3, 4, 350)}
        items = {ident: artifact_entry(ident, ident, "perf-telemetry-%d-1" % ident,
                                       data[ident]) for ident in data}
        # An already-kept newest result must not prevent an old artifact's admission.
        self.cycle([run_entry(350)], {350: [items[350]]})
        items[3]["expired"] = True
        items[4]["digest"] = "sha256:" + "0" * 64
        artifacts = {ident: [items[ident]] for ident in (1, 3, 4, 350)}
        observed = {}
        visited = set()
        for _ in range(3):
            doc, _ = self.cycle(runs, artifacts)
            visited.update(item["run_id"] for item in doc["runs"])
            observed.update((item["run_id"], item["outcome"]) for item in doc["runs"])
            self.assertLessEqual(len({item["run_id"] for item in doc["runs"]}), 137)
        self.assertEqual(visited, set(range(1, 351)))
        self.assertIn(observed[1], ("collected", "already_collected"))
        self.assertEqual([observed[ident] for ident in (2, 3, 4, 350)],
                         ["no_result", "expired", "refused", "already_collected"])
        # Retryable failures now have real verified bundles; the high run gets a rerun.
        items[3]["expired"] = False
        items[4]["digest"] = sha256_bytes(data[4])
        artifacts[2] = [items[2]]
        runs[-1] = run_entry(350, attempt=2)
        artifacts[350].append(artifact_entry(351, 350, "perf-telemetry-350-2", zipped(
            self.result(350, attempt=2))))
        for _ in range(3):
            doc, fake = self.cycle(runs, artifacts)
            self.assertLessEqual(len({item["run_id"] for item in doc["runs"]}), 137)
            self.assertFalse(any("/artifacts/1/zip" in url for url, _ in fake.requests))
        history = contracts.read_file(os.path.join(self.config["root"], "history.json"),
                                      "telemetry-history")
        self.assertEqual({item["result_id"] for item in history["results"]},
                         {"tl_1_1", "tl_2_1", "tl_3_1", "tl_4_1", "tl_350_1", "tl_350_2"})

    def test_deadline_preserves_position_and_restart_wraps_after_last_attempt(self):
        self.config["runs_max"] = 4
        runs = [run_entry(ident) for ident in range(1, 6)]
        remaining = transport.Collector.remaining

        def deadline(api, step):
            if step == "collecting run 3":
                raise ContractError("synthetic cycle deadline")
            return remaining(api, step)

        with mock.patch.object(transport.Collector, "remaining", deadline):
            doc, _ = self.cycle(runs, {})
        self.assertEqual([item["run_id"] for item in doc["runs"]], [1, 2])
        self.assertEqual(doc["status"], "failed")
        self.assertIn("synthetic cycle deadline", doc["detail"])
        position = os.path.join(self.config["root"], telemetrycollect.POSITION_JSON)
        self.assertEqual(contracts.read_file(position, "telemetry-collection-position")[
            "after_run_id"], 2)
        # Restart is a fresh Cycle/Collector/HistoryRoot, with a leftover killed write.
        with open(os.path.join(self.config["root"], telemetrylab.REPORT_STAGE,
                               telemetrycollect.POSITION_JSON), "wb") as stream:
            stream.write(b"interrupted")
        doc, _ = self.cycle(runs, {})
        self.assertEqual([item["run_id"] for item in doc["runs"]], [3, 4, 5, 1])
        self.assertFalse(os.path.exists(os.path.join(self.config["root"], telemetrylab.REPORT_STAGE,
                                                     telemetrycollect.POSITION_JSON)))
        # If the old position falls outside lookback, IDs alone still order and wrap.
        doc, _ = self.cycle([run_entry(8), run_entry(9)], {})
        self.assertEqual([item["run_id"] for item in doc["runs"]], [8, 9])
        self.assertEqual(doc["status"], "ok")
        doc, _ = self.cycle([], {})
        self.assertEqual(doc["status"], "ok")
        self.assertEqual(contracts.read_file(position, "telemetry-collection-position")[
            "after_run_id"], 9)

    def test_interruption_during_a_run_advances_attempt_position_without_losing_retry(self):
        self.config["runs_max"] = 1
        runs = [run_entry(ident) for ident in (1, 2)]
        original = telemetrycollect.artifacts

        def interrupted(api, run_id):
            raise ContractError("synthetic artifact deadline")

        with mock.patch.object(telemetrycollect, "artifacts", interrupted):
            doc, _ = self.cycle(runs, {})
        self.assertEqual([(item["run_id"], item["outcome"]) for item in doc["runs"]],
                         [(1, "refused")])
        doc, _ = self.cycle(runs, {})
        self.assertEqual([item["run_id"] for item in doc["runs"]], [2])
        with mock.patch.object(telemetrycollect, "artifacts", wraps=original) as listing:
            doc, _ = self.cycle(runs, {})
        self.assertEqual([item["run_id"] for item in doc["runs"]], [1])
        self.assertEqual(listing.call_args.args[1], 1)

    def test_failed_position_replacement_keeps_original_state_and_verified_evidence(self):
        self.config["runs_max"] = 1
        self.cycle([run_entry(100)], {})
        position = os.path.join(self.config["root"], telemetrycollect.POSITION_JSON)
        before = read(position)
        data = zipped(self.result(1))
        artifacts = {1: [artifact_entry(1, 1, "perf-telemetry-1-1", data)]}
        replace = os.replace

        def interrupted(source, target):
            if target == position:
                raise OSError("synthetic position write interruption")
            return replace(source, target)

        with mock.patch.object(os, "replace", interrupted):
            doc, _ = self.cycle([run_entry(1), run_entry(100)], artifacts)
        self.assertEqual(doc["status"], "failed")
        self.assertIn("synthetic position write interruption", doc["detail"])
        self.assertEqual(read(position), before)
        self.assertFalse(os.path.lexists(position + ".tmp"))
        self.assertEqual(doc["runs"][0]["outcome"], "collected")
        bundle = os.path.join(self.config["root"], "results", "tl_1_1")
        original = telemetrylab.read_bundle(bundle)[1]
        doc, fake = self.cycle([run_entry(1), run_entry(100)], artifacts)
        self.assertEqual(doc["runs"][0]["outcome"], "already_collected")
        self.assertFalse(any("/zip" in url for url, _ in fake.requests))
        self.assertEqual(telemetrylab.read_bundle(bundle)[1], original)

    def test_new_runs_cannot_extend_a_rotation_and_starve_old_missing_runs(self):
        self.config["runs_max"] = 1
        runs = [run_entry(1), run_entry(2)]
        observed = []
        for arriving in range(3, 8):
            doc, _ = self.cycle(runs, {})
            observed.append(doc["runs"][0]["run_id"])
            runs.append(run_entry(arriving))
        # A plain cursor would chase 1,2,3,4,5 forever. The first pass ends at 2;
        # the next is bounded at 4, so both old missing runs get another turn.
        self.assertEqual(observed, [1, 2, 1, 2, 3])
        # The fixed upper run disappears before it is attempted: wrap still occurs.
        runs = [run for run in runs if run["id"] != 4]
        doc, _ = self.cycle(runs, {})
        self.assertEqual([item["run_id"] for item in doc["runs"]], [1])

    def test_incomplete_or_changed_listings_never_process_a_prefix_or_move_position(self):
        self.cycle([run_entry(20)], {})
        position = os.path.join(self.config["root"], telemetrycollect.POSITION_JSON)
        before = read(position)
        page = FakeApi.page
        cases = (("changed count", 350), ("duplicate", 350), ("short", 350),
                 ("empty", 350), ("excess", 350), ("negative count", 350),
                 ("wrong workflow", 350), ("too many pages", 1001))
        for kind, count in cases:
            with self.subTest(kind=kind):
                def malformed(fake, url, key, rows):
                    doc = page(fake, url, key, rows)
                    if key != "workflow_runs":
                        return doc
                    number = int(parse_qs(urlsplit(url).query)["page"][0])
                    if kind == "changed count" and number == 2:
                        doc["total_count"] += 1
                    elif kind == "duplicate" and number == 2:
                        doc[key][0] = rows[0]
                    elif kind == "short" and number == 2:
                        doc[key].pop()
                    elif kind == "empty" and number == 2:
                        doc[key] = []
                    elif kind == "excess" and number == 2:
                        doc[key].append(rows[0])
                    elif kind == "negative count":
                        doc["total_count"] = -1
                    elif kind == "wrong workflow" and number == 4:
                        doc[key][0] = dict(doc[key][0], workflow_id=WORKFLOW_ID + 1)
                    return doc

                with mock.patch.object(FakeApi, "page", malformed):
                    doc, fake = self.cycle([run_entry(i) for i in range(1, count + 1)], {})
                self.assertEqual(doc["status"], "failed", doc)
                self.assertEqual(doc["runs"], [])
                self.assertEqual(read(position), before)
                self.assertNotIn("rest wait", doc["detail"])
                self.assertFalse(any("/artifacts?" in url for url, _ in fake.requests))
                self.assertLessEqual(sum("/runs?" in url for url, _ in fake.requests),
                                     telemetrycollect.PAGES_MAX)
                if kind == "too many pages":
                    self.assertIn("narrow the lookback", doc["detail"])

    def test_the_existing_page_bound_is_inclusive_and_processing_stays_capped(self):
        self.config["runs_max"] = 1
        doc, fake = self.cycle([run_entry(i) for i in range(1, 1001)], {})
        self.assertEqual([item["run_id"] for item in doc["runs"]], [1])
        self.assertEqual(sum("/runs?" in url for url, _ in fake.requests), 10)
        self.assertEqual(sum("/artifacts?" in url for url, _ in fake.requests), 1)

    def test_position_is_bounded_versioned_bound_to_identity_and_never_follows_links(self):
        self.cycle([run_entry(1)], {})
        path = os.path.join(self.config["root"], telemetrycollect.POSITION_JSON)
        good = read(path)
        doc = contracts.read_json(good, "telemetry-collection-position")
        for mutation in ({"schema": "af.perf-telemetry-collection-position/0"},
                         {"schema": "af.perf-telemetry-collection-position/2"},
                         {"repository": "another/repo"}, {"workflow_id": WORKFLOW_ID + 1},
                         {"after_run_id": 0}, {"through_run_id": 0},
                         {"after_run_id": 2, "through_run_id": 1}, {"extra": "refused"}):
            with self.subTest(mutation=mutation):
                with open(path, "wb") as stream:
                    stream.write(json.dumps(dict(doc, **mutation)).encode())
                refused, fake = self.cycle([run_entry(2)], {})
                self.assertEqual(refused["status"], "failed")
                self.assertEqual(refused["runs"], [])
                self.assertFalse(any("/runs?" in url for url, _ in fake.requests))
        with open(path, "wb") as stream:
            stream.write(b" " * (telemetrycollect.POSITION_BYTES_MAX + 1))
        refused, _ = self.cycle([run_entry(2)], {})
        self.assertIn("byte bound", refused["detail"])
        os.unlink(path)
        outside = self.path("outside-position")
        with open(outside, "wb") as stream:
            stream.write(good)
        os.symlink(outside, path)
        refused, _ = self.cycle([run_entry(2)], {})
        self.assertEqual(refused["status"], "failed")
        self.assertEqual(refused["runs"], [])
        self.assertEqual(read(outside), good)
        os.unlink(path)
        os.mkfifo(path)
        refused, _ = self.cycle([run_entry(2)], {})
        self.assertIn("not a regular file", refused["detail"])
        # Explicit operator reset (under no running collector) starts oldest first again.
        os.unlink(path)
        doc, _ = self.cycle([run_entry(2), run_entry(3)], {})
        self.assertEqual([item["run_id"] for item in doc["runs"]], [2, 3])

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

    def test_partial_and_failed_measurements_are_collected_but_tampered_evidence_is_refused(self):
        def negative(doc):
            doc["drain"]["pairs"][0]["off_ms"] = -0.125

        artifacts = {}
        for run_id, sides in ((101, ("candidate",)), (102, ("baseline", "candidate"))):
            executor = fx.StandIn(self.binaries, {
                ("overhead", side): {"extra": negative} for side in sides})
            with mock.patch.object(fx, "StandIn", return_value=executor):
                path = self.result(run_id, rounds=1)
            data = zipped(path)
            artifacts[run_id] = [artifact_entry(run_id, run_id,
                                               "perf-telemetry-%d-1" % run_id, data)]
        doc, _ = self.cycle([run_entry(101), run_entry(102)], artifacts)
        self.assertEqual(doc["status"], "ok", doc)
        self.assertTrue(all(run["outcome"] == "collected" for run in doc["runs"]), doc)
        history = contracts.read_file(os.path.join(self.config["root"], "history.json"),
                                      "telemetry-history")
        self.assertEqual({run["result_id"]: run["status"] for run in history["results"]},
                         {"tl_101_1": "partial", "tl_102_1": "failed"})

        executor = fx.StandIn(self.binaries, {
            ("overhead", side): {"extra": negative} for side in ("baseline", "candidate")})
        with mock.patch.object(fx, "StandIn", return_value=executor):
            path = self.result(103, rounds=1)
        result = telemetrylab.read_bundle(path)[0]
        run = result["runs"][0]
        run["measurement_refusal"].update(token="0.125", token_bytes=5,
                                          token_sha256=sha256_bytes(b"0.125"))
        run["detail"] = telemetrylab.measurement_detail(run, run["measurement_refusal"])
        fx.rewrite(path, result)
        data = zipped(path)
        doc, _ = self.cycle([run_entry(103)], {
            103: [artifact_entry(103, 103, "perf-telemetry-103-1", data)]})
        self.assertEqual(doc["runs"][0]["outcome"], "refused", doc)
        history = contracts.read_file(os.path.join(self.config["root"], "history.json"),
                                      "telemetry-history")
        self.assertEqual(len(history["results"]), 2)

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

    def test_retained_binding_is_required_for_offline_history_and_repeated_collection(self):
        data = zipped(self.result(302))
        artifacts = {302: [artifact_entry(302, 302, "perf-telemetry-302-1", data)]}
        doc, _ = self.cycle([run_entry(302)], artifacts)
        self.assertEqual(doc["runs"][0]["outcome"], "collected", doc)
        binding_path = os.path.join(self.config["root"], telemetrylab.BINDINGS, "tl_302_1.json")
        original = read(binding_path)
        binding = json.loads(original)
        for changed in (None, dict(binding, candidate_sha="a" * 40),
                        dict(binding, attempt=2)):
            with self.subTest(changed=changed):
                if changed is None:
                    os.unlink(binding_path)
                else:
                    with open(binding_path, "w") as stream:
                        json.dump(changed, stream)
                with telemetrylab.HistoryRoot(self.config["root"]) as kept:
                    history = kept.write()
                self.assertEqual(history["results"], [])
                self.assertTrue(history["refused"])
                doc, _ = self.cycle([run_entry(302)], artifacts)
                self.assertEqual(doc["runs"][0]["outcome"], "refused")
                with open(binding_path, "wb") as stream:
                    stream.write(original)

    def test_missing_original_plan_and_changed_sources_are_refused_before_import(self):
        directory = self.result(301)
        result, _, _ = telemetrylab.read_bundle(directory)
        original = copy.deepcopy(result)
        run = run_entry(301, head_sha=self.authority)
        binding = dict(self.planned[(301, 1)], schema=contracts.TELEMETRY_BINDING_SCHEMA,
                       repository=REPOSITORY, run_id=301, attempt=1,
                       event="workflow_dispatch", event_sha=self.authority,
                       run_head_sha=self.authority)
        for change in ("missing", "candidate_sha", "baseline_sha", "authority_sha",
                       "run_id", "attempt", "repository", "workflow", "foreign-files"):
            with self.subTest(change=change):
                result = copy.deepcopy(original)
                receipt = copy.deepcopy(binding)
                if change in ("candidate_sha", "baseline_sha", "authority_sha"):
                    result["sources"][change] = "a" * 40
                elif change in ("run_id", "attempt"):
                    receipt[change] += 1
                elif change == "repository":
                    receipt["repository"] = "foreign/repo"
                elif change == "workflow":
                    receipt["event_sha"] = self.untrusted
                fx.rewrite(directory, result)
                listed = [artifact_entry(301, 301, "perf-telemetry-301-1", zipped(directory))]
                if change != "missing":
                    data = binding_zip(receipt)
                    if change == "foreign-files":
                        data = zipped(directory)
                    listed.append(artifact_entry(1301, 301, "telemetry-plan-301-1", data))
                with mock.patch.object(telemetrylab.HistoryRoot, "import_result") as imported:
                    doc, _ = self.cycle([run], {301: listed}, add_plans=False)
                self.assertEqual(doc["runs"][0]["outcome"], "refused", doc)
                self.assertFalse(imported.called)
        fx.rewrite(directory, original)

    def test_pr_merge_binding_ignores_later_mutable_pr_heads_and_preserves_attempts(self):
        # Actual Git commits model GitHub's two different SHAs: run.head_sha is the
        # original branch head, GITHUB_SHA is the measured merge with original base first.
        self.repo.git("checkout", "-q", "-b", "feature")
        head = self.repo.commit("original PR push", {"feature": "one"})
        self.repo.git("checkout", "-q", "main")
        self.repo.git("merge", "--no-ff", "-m", "original event merge", "feature")
        merge = self.repo.git("rev-parse", "HEAD")
        self.repo.git("checkout", "-q", "feature")
        later = self.repo.commit("later PR push", {"feature": "two"})
        run = run_entry(40206196989, attempt=2, event="pull_request", head_sha=head)
        run["pull_requests"] = [{"head": {"sha": later}, "base": {"sha": merge}}]
        sources = {"candidate_sha": merge, "baseline_sha": self.authority,
                   "authority_sha": self.authority}
        result = {"result_id": "tl_40206196989_1", "trigger": "pr", "profile": "pr",
                  "sources": sources}
        binding = {"schema": contracts.TELEMETRY_BINDING_SCHEMA, "repository": REPOSITORY,
                   "run_id": run["id"], "attempt": 1, "event": "pull_request",
                   "event_sha": merge, "run_head_sha": head, "candidate_sha": merge,
                   "baseline_sha": self.authority, "control_sha": self.authority, "profile": "pr"}
        cycle = telemetrycollect.Cycle(self.config, transport.Collector(
            FakeApi([], {}), REPOSITORY, TOKEN, api_url=API), gitrefs.Git(self.repo.path),
            time.time, NOW)
        cycle.bind(run, 1, result, binding)  # Original attempt remains valid after later push.
        result2 = dict(result, result_id="tl_40206196989_2")
        cycle.bind(run, 2, result2, dict(binding, attempt=2))
        for key in sources:
            forged = dict(result, sources=dict(sources, **{key: head}))
            with self.subTest(key=key), self.assertRaises(ContractError):
                cycle.bind(run, 1, forged, binding)
        for mutation in ({"attempt": 2}, {"run_id": 40209146633}, {"run_head_sha": later},
                         {"candidate_sha": head}, {"baseline_sha": merge},
                         {"control_sha": merge}):
            with self.subTest(mutation=mutation), self.assertRaises(ContractError):
                cycle.bind(run, 1, result, dict(binding, **mutation))
        # Altering the workflow at the event source cannot manufacture trusted planning.
        bad = self.repo.commit("foreign workflow", {
            ".github/workflows/performance-telemetry.yml": "foreign producer"})
        manual = dict(result, trigger="manual")
        receipt = dict(binding, event="workflow_dispatch", event_sha=bad, run_head_sha=bad)
        with self.assertRaisesRegex(ContractError, "trusted installed producer"):
            cycle.bind(dict(run, event="workflow_dispatch", head_sha=bad), 1, manual, receipt)

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
