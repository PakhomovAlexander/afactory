"""Release-performance handoff and deduplication (ADR-0180).

A stand-in GitHub API answers the handoff's read-only requests over the test history of
tests.test_planning (v0.1.0 at R, v0.2.0-rc.1 at C1, v0.2.0 at C2): the upstream Release run, its
attempts' jobs, the tag and its release, and the claim history with each claim artifact's ZIP.
Nothing here is a hosted run, an authenticated request or an actual artifact transfer; these
tests establish the decisions, not the hosted handoff, which stays pending until an authorized
staging run (docs/performance-ci.md).
"""
import io
import json
import zipfile
from urllib.parse import parse_qs, urlsplit

from perflab import ci, contracts, release, transport
from tests import test_planning

API = "https://api.example.invalid"
STORAGE = "https://storage.example.invalid"
REPOSITORY = "owner/af"
REPOSITORY_ID = 31
RELEASE_WORKFLOW_ID = 61
HANDOFF_WORKFLOW_ID = 62
UPSTREAM = 7001
NOW_MS = 1792490400000  # 2026-10-19T16:00:00Z
NOW = NOW_MS / 1000.0
IMAGE = "registry.example.invalid/lab/rust@sha256:" + "4" * 64
MUSL = "registry.example.invalid/lab/zigbuild@sha256:" + "5" * 64


def stamp(unix_ms):
    from datetime import datetime, timedelta
    return (datetime(1970, 1, 1) + timedelta(milliseconds=unix_ms)).strftime(
        "%Y-%m-%dT%H:%M:%SZ")


def job(job_id, name, conclusion, run_id=UPSTREAM, attempt=1, sha=None, status="completed"):
    return {"id": job_id, "name": name, "run_id": run_id, "run_attempt": attempt,
            "head_sha": sha, "status": status, "conclusion": conclusion,
            "workflow_name": "Release"}


def claim_zip(doc, members=None):
    data = io.BytesIO()
    with zipfile.ZipFile(data, "w", zipfile.ZIP_DEFLATED) as container:
        for name, body in (members or [(release.CLAIM_FILE,
                                        contracts.write_bytes(doc, "release-claim"))]):
            container.writestr(name, body)
    return data.getvalue()


class FakeGitHub(object):
    """The repository's API as the handoff reads it, and the signed storage its artifact
    downloads redirect to; records every request and its headers."""

    def __init__(self, sha):
        self.sha = sha
        self.requests = []
        self.overrides = {}
        self.workflow = {"id": RELEASE_WORKFLOW_ID, "path": release.RELEASE_WORKFLOW_PATH,
                         "name": "Release"}
        self.runs = {}
        self.jobs = {}
        self.tags = {"v0.2.0": {"type": "tag", "sha": "a" * 40}}
        self.annotated = {"a" * 40: {"type": "commit", "sha": sha}}
        self.releases = {"v0.2.0": {"id": 501, "tag_name": "v0.2.0", "draft": False,
                                    "prerelease": False,
                                    "published_at": stamp(NOW_MS - 3600 * 1000)}}
        self.artifacts = []
        self.next_artifact = 8800
        self.handoff_runs = {}
        self.total_drift = {}

    # The upstream run -------------------------------------------------------------------------

    def upstream(self, run_id=UPSTREAM, attempts=1, branch="main", event="push", sha=None,
                 workflow_id=RELEASE_WORKFLOW_ID, path=release.RELEASE_WORKFLOW_PATH,
                 repository=REPOSITORY, head_repository=REPOSITORY, created_ms=None,
                 conclusion="success"):
        run = {"id": run_id, "name": "Release", "run_attempt": attempts,
               "workflow_id": workflow_id, "path": path, "head_branch": branch,
               "head_sha": sha or self.sha, "event": event, "status": "completed",
               "conclusion": conclusion, "created_at": stamp(created_ms or NOW_MS - 7200000),
               "repository": {"full_name": repository},
               "head_repository": {"full_name": head_repository}}
        self.runs[run_id] = run
        return run

    def published(self, run_id=UPSTREAM, attempt=1, publish="success", pin="success",
                  extra=0, first_id=1):
        jobs = [job(first_id + index, "build target %d" % index, "success", run_id, attempt,
                    self.sha) for index in range(extra)]
        if publish is not None:
            jobs.append(job(first_id + extra, release.PUBLISH_JOB, publish, run_id, attempt,
                            self.sha))
        if pin is not None:
            jobs.append(job(first_id + extra + 1, release.PIN_JOB, pin, run_id, attempt,
                            self.sha))
        self.jobs[(run_id, attempt)] = jobs

    def payload(self, run_id=UPSTREAM, **changes):
        run = json.loads(json.dumps(self.runs[run_id]))
        run.update(changes)
        return {"workflow_run": run}

    # The claim history ------------------------------------------------------------------------

    def handoff_run(self, run_id, branch="main", path=release.HANDOFF_WORKFLOW_PATH):
        self.handoff_runs[run_id] = {"id": run_id, "path": path, "head_branch": branch,
                                     "repository": {"full_name": REPOSITORY},
                                     "workflow_id": HANDOFF_WORKFLOW_ID}

    def upload(self, name, data, run_id, created_ms=NOW_MS, expired=False, branch="main",
               repository_id=REPOSITORY_ID, path=release.HANDOFF_WORKFLOW_PATH):
        """An artifact as `upload-artifact` would leave it, written by Actions run `run_id`."""
        if run_id not in self.handoff_runs:
            self.handoff_run(run_id, branch, path)
        self.next_artifact += 1
        item = {"id": self.next_artifact, "name": name, "size_in_bytes": len(data),
                "expired": expired, "created_at": stamp(created_ms),
                "expires_at": stamp(created_ms + 90 * release.DAY_MS),
                "workflow_run": {"id": run_id, "repository_id": repository_id,
                                 "head_repository_id": repository_id, "head_branch": branch,
                                 "head_sha": "f" * 40},
                "data": data}
        self.artifacts.append(item)
        return item

    # The transport ----------------------------------------------------------------------------

    def answer(self, status, doc):
        return transport.Response(status, {}, json.dumps(doc).encode())

    def page(self, url, items, key):
        query = parse_qs(urlsplit(url).query)
        page, size = int(query["page"][0]), int(query["per_page"][0])
        total = len(items) + self.total_drift.get((urlsplit(url).path, page), 0)
        return self.answer(200, {"total_count": total,
                                 key: items[(page - 1) * size:page * size]})

    def request(self, url, headers, limit, timeout):
        self.requests.append((url, dict(headers)))
        if url in self.overrides:
            return self.overrides[url]
        base = "%s/repos/%s" % (API, REPOSITORY)
        path = urlsplit(url).path
        if url.startswith(STORAGE):
            ident = int(path.rsplit("/", 1)[1])
            for item in self.artifacts:
                if item["id"] == ident:
                    return transport.Response(200, {}, item["data"][:limit + 1])
            return transport.Response(404, {}, b"")
        if not url.startswith(base):
            return transport.Response(404, {}, b"")
        rest = url[len(base):]
        if rest == "":
            return self.answer(200, {"id": REPOSITORY_ID, "full_name": REPOSITORY})
        if rest == "/actions/workflows/release.yml":
            return self.answer(200, self.workflow) if self.workflow else self.answer(404, {})
        parts = path[len("/repos/%s/" % REPOSITORY):].split("/")
        if parts[:2] == ["actions", "runs"] and len(parts) == 3:
            ident = int(parts[2])
            run = self.runs.get(ident) or self.handoff_runs.get(ident)
            return self.answer(200, run) if run else self.answer(404, {})
        if parts[:2] == ["actions", "runs"] and parts[3:4] == ["attempts"]:
            return self.page(url, self.jobs.get((int(parts[2]), int(parts[4])), []), "jobs")
        if parts[:2] == ["actions", "runs"] and parts[3:4] == ["artifacts"]:
            ident = int(parts[2])
            listed = [dict((key, value) for key, value in item.items() if key != "data")
                      for item in self.artifacts if item["workflow_run"]["id"] == ident]
            return self.page(url, listed, "artifacts")
        if parts[:2] == ["actions", "artifacts"] and len(parts) == 2:
            name = parse_qs(urlsplit(url).query)["name"][0]
            listed = [dict((key, value) for key, value in item.items() if key != "data")
                      for item in self.artifacts if item["name"] == name]
            return self.page(url, listed, "artifacts")
        if parts[:2] == ["actions", "artifacts"] and parts[3:4] == ["zip"]:
            ident = int(parts[2])
            for item in self.artifacts:
                if item["id"] == ident:
                    if item["expired"]:
                        return transport.Response(410, {}, b"")
                    return transport.Response(302, {"Location": "%s/blob/%d" % (STORAGE,
                                                                               ident)}, b"")
            return transport.Response(404, {}, b"")
        if parts[:3] == ["git", "ref", "tags"]:
            found = self.tags.get(parts[3])
            return self.answer(200, {"ref": "refs/tags/" + parts[3], "object": found}) \
                if found else self.answer(404, {})
        if parts[:2] == ["git", "tags"]:
            return self.answer(200, {"object": self.annotated[parts[2]]})
        if parts[:2] == ["releases", "tags"]:
            found = self.releases.get(parts[2])
            return self.answer(200, found) if found else self.answer(404, {})
        return transport.Response(404, {}, b"")


class Handoff(test_planning.History):

    def setUp(self):
        test_planning.History.setUp(self)
        self.github = FakeGitHub(self.c2)
        self.github.upstream()
        self.github.published(pin="failure")

    def api(self):
        return release.Api(self.github, REPOSITORY, "read-only-token", api_url=API)

    def actions(self, run_id=9100, attempt=1, event="workflow_run"):
        return release.Actions(REPOSITORY, run_id, attempt, event, self.c3)

    def handoff(self, run_id=9100, attempt=1, payload=None, remeasure=None, wall=NOW):
        event = "workflow_run" if remeasure is None else "workflow_dispatch"
        doc, claim = release.handoff(self.api(), self.git, self.actions(run_id, attempt, event),
                                     payload=payload if payload is not None
                                     else self.github.payload(), remeasure=remeasure,
                                     wall=lambda: wall)
        # Every decision the handoff writes passes its own reader.
        self.assertEqual(contracts.read_json(contracts.write_bytes(doc, "release-handoff"),
                                             "release-handoff"), doc)
        self.assertEqual(claim is not None, doc["status"] == "claimed")
        return doc, claim

    def write_claim(self, doc, claim, created_ms=NOW_MS):
        """What the workflow's upload step does with a claimed handoff."""
        return self.github.upload(doc["claim_artifact"], claim_zip(claim),
                                  claim["claimant"]["actions_run_id"], created_ms)

    def confirm(self, doc, claim):
        confirmed = release.confirm(self.api(), doc, claim)
        self.assertEqual(contracts.read_json(contracts.write_bytes(confirmed, "release-handoff"),
                                             "release-handoff"), confirmed)
        return confirmed

    def claimed(self, run_id=9100, attempt=1, created_ms=NOW_MS):
        doc, claim = self.handoff(run_id, attempt)
        self.assertEqual(doc["status"], "claimed", doc["detail"])
        self.write_claim(doc, claim, created_ms)
        return doc, claim

    def result(self, run_id=9100, attempt=1, role="control_bootstrap", expired=False):
        """The run bundle the claiming run's aggregate job uploads."""
        return self.github.upload("af-perf-gh%d_a%d_%s-run" % (run_id, attempt, role), b"PK",
                                  run_id, expired=expired)

    def decided(self, doc, status, reason):
        self.assertEqual((doc["status"], doc["reason"]), (status, reason), doc["detail"])

    # Exact upstream identity ------------------------------------------------------------------

    def test_a_published_main_push_is_claimed_by_its_run_tag_and_sha_without_the_attempt(self):
        self.github.upstream(attempts=2, conclusion="failure")
        self.github.published(attempt=2, publish=None, pin="failure", first_id=50)
        doc, claim = self.handoff(payload=self.github.payload(run_attempt=2))
        self.decided(doc, "claimed", None)
        identity = {"repository": REPOSITORY, "upstream_run_id": UPSTREAM, "tag": "v0.2.0",
                    "sha": self.c2}
        self.assertEqual(claim["identity"], identity)
        self.assertEqual(claim["key"], release.identity_key(identity))
        # The attempt is not part of the identity: attempt 1's own event claims the same key.
        first, _ = self.handoff(payload=self.github.payload(run_attempt=1))
        self.assertEqual((first["status"], first["key"]), ("claimed", claim["key"]))
        self.assertEqual(doc["claim_artifact"], "af-release-claim-" + claim["key"][7:])
        self.assertFalse(doc["claim_artifact"].startswith(transport.ARTIFACT_PREFIX),
                         "a claim is never a transport artifact the collector imports")
        self.assertEqual(claim["claimant"], {
            "repository": REPOSITORY, "workflow_path": release.HANDOFF_WORKFLOW_PATH,
            "actions_run_id": 9100, "actions_run_attempt": 1, "lab_run_prefix": "gh9100_a1_"})
        self.assertEqual((claim["upstream"]["run_attempt"],
                          claim["upstream"]["published_in_attempt"]), (2, 1))
        self.assertEqual(doc["release"], {"tag": "v0.2.0", "sha": self.c2, "id": 501,
                                          "prerelease": False,
                                          "published_at": stamp(NOW_MS - 3600 * 1000)})
        self.assertEqual(doc["enforcement"], "advisory")
        # Every request carried the token only to the API; nothing was downloaded or written.
        for url, headers in self.github.requests:
            self.assertTrue(url.startswith(API), url)
            self.assertEqual(headers["Authorization"], "Bearer read-only-token")
        self.assertFalse(any("/zip" in url for url, _ in self.github.requests))

    def test_a_pushed_release_tag_is_its_own_release(self):
        self.github.upstream(branch="v0.2.0")
        doc, claim = self.handoff()
        self.decided(doc, "claimed", None)
        self.assertEqual(claim["identity"]["tag"], "v0.2.0")

    def test_a_workflow_named_release_that_is_not_release_yml_supplies_no_provenance(self):
        for changes in ({"workflow_id": 99}, {"path": ".github/workflows/other.yml"},
                        {"workflow_id": 99, "path": ".github/workflows/other.yml"}):
            with self.subTest(changes=changes):
                # Its name is Release and it has a successful job named `sign and publish`.
                doc, _ = self.handoff(payload=self.github.payload(**changes))
                self.decided(doc, "not_eligible", "wrong_workflow")
        self.github.workflow = None
        doc, _ = self.handoff()
        self.decided(doc, "not_eligible", "wrong_workflow")

    def test_another_repository_or_a_fork_head_is_not_eligible(self):
        for changes in ({"repository": {"full_name": "someone/af"}},
                        {"head_repository": {"full_name": "someone/af"}}):
            with self.subTest(changes=changes):
                doc, _ = self.handoff(payload=self.github.payload(**changes))
                self.decided(doc, "not_eligible", "wrong_repository")

    def test_identities_the_api_contradicts_refuse_rather_than_conclude(self):
        doc, _ = self.handoff(payload=self.github.payload(head_sha=self.c1))
        self.decided(doc, "refused", "identity_mismatch")
        self.github.jobs[(UPSTREAM, 1)][0]["head_sha"] = self.c1
        doc, _ = self.handoff()
        self.decided(doc, "refused", "identity_mismatch")
        self.github.published(pin="failure")
        self.github.jobs[(UPSTREAM, 1)][0]["run_id"] = UPSTREAM + 1
        doc, _ = self.handoff()
        self.decided(doc, "refused", "identity_mismatch")
        self.github.published(pin="failure")
        self.github.annotated["a" * 40] = {"type": "commit", "sha": self.c1}
        doc, _ = self.handoff()
        self.decided(doc, "refused", "tag_mismatch")

    def test_a_main_push_without_a_published_release_tag_is_not_eligible(self):
        self.github.upstream(sha=self.c3)
        self.github.sha = self.c3
        self.github.published()
        doc, _ = self.handoff()
        self.decided(doc, "not_eligible", "no_published_tag")
        self.assertIsNone(doc["identity"])

    def test_only_a_published_nondraft_release_of_the_exact_tag_is_eligible(self):
        self.github.releases["v0.2.0"]["draft"] = True
        doc, _ = self.handoff()
        self.decided(doc, "not_eligible", "draft_release")
        del self.github.releases["v0.2.0"]
        doc, _ = self.handoff()
        self.decided(doc, "not_eligible", "not_published")
        self.github.releases["v0.2.0"] = {"id": 501, "tag_name": "v0.2.0", "draft": False,
                                          "prerelease": True, "published_at": stamp(NOW_MS)}
        doc, _ = self.handoff()
        self.decided(doc, "refused", "prerelease_mismatch")

    def test_only_a_completed_push_run_is_eligible(self):
        doc, _ = self.handoff(payload=self.github.payload(status="in_progress"))
        self.decided(doc, "not_eligible", "not_completed")
        self.github.upstream(event="workflow_dispatch")
        doc, _ = self.handoff()
        self.decided(doc, "not_eligible", "not_push")
        doc, _ = self.handoff(payload={"workflow_run": {"id": "7001"}})
        self.decided(doc, "refused", "invalid_event")

    # Every attempt, completely --------------------------------------------------------------

    def test_publication_then_a_failed_pin_retry_stays_eligible(self):
        self.github.upstream(attempts=2, conclusion="failure")
        self.github.published(attempt=1, publish="success", pin="failure")
        self.github.published(attempt=2, publish=None, pin="failure", first_id=50)
        doc, claim = self.handoff(payload=self.github.payload(run_attempt=2))
        self.decided(doc, "claimed", None)
        self.assertEqual(doc["publication"], {
            "attempts": [{"attempt": 1, "jobs": 2, "publish": "success", "pin": "failure"},
                         {"attempt": 2, "jobs": 1, "publish": None, "pin": "failure"}],
            "published_in_attempt": 1})
        listed = [url for url, _ in self.github.requests if "/attempts/" in url]
        self.assertEqual(listed, [
            API + "/repos/%s/actions/runs/%d/attempts/%d/jobs?per_page=100&page=1"
            % (REPOSITORY, UPSTREAM, attempt) for attempt in (1, 2)])

    def test_no_successful_publication_in_any_attempt_is_not_eligible(self):
        self.github.upstream(attempts=2)
        self.github.published(attempt=1, publish="failure", pin=None)
        self.github.published(attempt=2, publish="cancelled", pin=None, first_id=50)
        doc, _ = self.handoff()
        self.decided(doc, "not_eligible", "publish_not_successful")

    def test_job_pages_are_read_completely_and_bounded(self):
        # The publish job is on the second page of 150 jobs.
        self.github.published(extra=148)
        doc, _ = self.handoff()
        self.decided(doc, "claimed", None)
        self.assertEqual(doc["publication"]["attempts"][0]["jobs"], 150)
        # More jobs than three pages hold is incomplete, never "not published".
        self.github.published(extra=299)
        doc, _ = self.handoff()
        self.decided(doc, "incomplete", "jobs_incomplete")
        # A listing whose total changes while it is read is incomplete too.
        self.github.published(extra=148)
        self.github.total_drift[("/repos/%s/actions/runs/%d/attempts/1/jobs"
                                 % (REPOSITORY, UPSTREAM), 2)] = 1
        doc, _ = self.handoff()
        self.decided(doc, "incomplete", "jobs_incomplete")
        self.github.total_drift.clear()
        self.github.upstream(attempts=release.ATTEMPTS_MAX + 1)
        doc, _ = self.handoff()
        self.decided(doc, "incomplete", "jobs_incomplete")

    def test_an_unavailable_api_is_incomplete(self):
        self.github.overrides[API + "/repos/%s/actions/runs/%d" % (REPOSITORY, UPSTREAM)] = \
            transport.Response(502, {}, b"bad gateway")
        doc, _ = self.handoff()
        self.decided(doc, "incomplete", "api_unavailable")

    # Deduplication ----------------------------------------------------------------------------

    def test_the_first_valid_claim_measures(self):
        doc, claim = self.claimed()
        confirmed = self.confirm(doc, claim)
        self.decided(confirmed, "measure", None)
        self.assertEqual(confirmed["stage"], "confirm")
        self.assertEqual(confirmed["first_claim"]["actions_run_id"], 9100)
        self.assertEqual(release.outputs(confirmed), {
            "claim": "false", "claim_artifact": doc["claim_artifact"], "measure": "true"})
        self.assertEqual(release.outputs(doc)["claim"], "true")

    def test_claims_from_elsewhere_are_recorded_and_never_suppress_the_release(self):
        doc, claim = self.handoff()
        name = doc["claim_artifact"]
        forged = json.loads(json.dumps(claim))
        forged["claimant"].update(actions_run_id=500, lab_run_prefix="gh500_a1_")
        # A pull request's run, another repository's run and another workflow on main: their
        # producers are established as foreign, so none is a claim of this release.
        self.github.upload(name, claim_zip(forged), 500, NOW_MS - 50000, branch="topic")
        self.github.upload(name, claim_zip(None, [("claim.json", b"{}")]), 505,
                           NOW_MS - 45000, repository_id=REPOSITORY_ID + 1)
        self.github.upload(name, claim_zip(forged), 501, NOW_MS - 40000,
                           path=".github/workflows/ci.yml")
        doc, claim = self.claimed()
        self.assertEqual([(entry["valid"], entry["reason"]) for entry in doc["history"]["claims"]],
                         [(False, "foreign_run"), (False, "foreign_run"),
                          (False, "foreign_workflow")])
        confirmed = self.confirm(doc, claim)
        self.decided(confirmed, "measure", None)
        self.assertEqual([entry["valid"] for entry in confirmed["history"]["claims"]],
                         [False, False, False, True])

    def test_unreadable_trusted_claim_evidence_is_incomplete_and_never_remeasured(self):
        # An artifact of the claim's exact name that this repository's handoff workflow wrote
        # from main may be an earlier claim of this release: unreadable bytes, an invalid typed
        # claim, an oversized one or a claim of another identity cannot prove that it is not.
        # The handoff writes no claim, the evidence is retained, and nothing measures.
        def not_zip(github, name, claim):
            return github.upload(name, b"not a zip", 502, NOW_MS - 30000)

        def invalid_claim(github, name, claim):
            return github.upload(name, claim_zip(None, [("claim.json", b"{}")]), 502,
                                 NOW_MS - 30000)

        def oversized(github, name, claim):
            item = github.upload(name, claim_zip(claim), 502, NOW_MS - 30000)
            item["size_in_bytes"] = release.CLAIM_ARTIFACT_BYTES_MAX + 1
            return item

        def other_identity(github, name, claim):
            other = json.loads(json.dumps(claim))
            other["identity"]["upstream_run_id"] = UPSTREAM + 1
            other["key"] = release.identity_key(other["identity"])
            other["claimant"].update(actions_run_id=502, lab_run_prefix="gh502_a1_")
            return github.upload(name, claim_zip(other), 502, NOW_MS - 30000)

        def other_claimant(github, name, claim):
            # A typed claim of this identity whose claimant is not the run that uploaded it.
            return github.upload(name, claim_zip(claim), 502, NOW_MS - 30000)

        cases = (("not_zip", not_zip, "unreadable"), ("invalid", invalid_claim, "unreadable"),
                 ("oversized", oversized, "oversized"),
                 ("other_identity", other_identity, "other_identity"),
                 ("other_claimant", other_claimant, "other_identity"))
        for label, plant, reason in cases:
            with self.subTest(case=label):
                self.setUp()
                probe, claim = self.handoff(9000)
                self.decided(probe, "claimed", None)
                plant(self.github, probe["claim_artifact"], claim)
                doc, written = self.handoff()
                self.decided(doc, "incomplete", "history_unavailable")
                self.assertIsNone(written)
                self.assertEqual(release.outputs(doc), {"claim": "false", "claim_artifact": "",
                                                        "measure": "false"})
                self.assertEqual([(entry["valid"], entry["reason"])
                                  for entry in doc["history"]["claims"]], [(False, reason)])
                self.assertEqual(doc["history"]["state"], "unavailable")
                self.assertEqual([item["name"] for item in self.github.artifacts],
                                 [probe["claim_artifact"]], "nothing more was claimed")

    def test_trusted_evidence_planted_after_a_claim_blocks_its_confirmation(self):
        # The real sequence: the handoff claims, the workflow uploads the claim, then confirm
        # reads the history again. A trusted unreadable artifact of the same name that predates
        # the claim may be the first claim, so confirmation measures nothing.
        doc, claim = self.handoff()
        self.decided(doc, "claimed", None)
        self.github.upload(doc["claim_artifact"], b"not a zip", 502, NOW_MS - 30000)
        self.write_claim(doc, claim)
        confirmed = self.confirm(doc, claim)
        self.decided(confirmed, "incomplete", "history_unavailable")
        self.assertEqual(release.outputs(confirmed)["measure"], "false")
        self.assertEqual([(entry["valid"], entry["reason"])
                          for entry in confirmed["history"]["claims"]],
                         [(False, "unreadable"), (True, None)])
        self.assertIn("the claim stays without a result", confirmed["detail"])
        # Without that artifact the same upload is confirmed and measures.
        self.setUp()
        doc, claim = self.claimed()
        self.decided(self.confirm(doc, claim), "measure", None)

    def test_a_duplicate_race_measures_only_the_earlier_claim(self):
        # Two runs of the same identity both found no claim and both wrote theirs.
        first, first_claim = self.handoff(9100)
        second, second_claim = self.handoff(9200)
        self.assertEqual((first["status"], second["status"]), ("claimed", "claimed"))
        self.write_claim(second, second_claim, NOW_MS + 2000)
        self.write_claim(first, first_claim, NOW_MS + 1000)
        self.decided(self.confirm(first, first_claim), "measure", None)
        lost = self.confirm(second, second_claim)
        self.decided(lost, "incomplete", "claim_without_result")
        self.assertIn("another run claimed this release first", lost["detail"])
        self.assertEqual(lost["first_claim"]["actions_run_id"], 9100)
        self.assertEqual(release.outputs(lost)["measure"], "false")
        # Once the winner's run bundle is retained, the loser links it.
        self.result(9100)
        linked = self.confirm(second, second_claim)
        self.decided(linked, "linked", None)
        self.assertEqual([item["name"] for item in linked["results"]],
                         ["af-perf-gh9100_a1_control_bootstrap-run"])

    def test_a_pin_only_rerun_links_the_retained_result(self):
        doc, claim = self.claimed()
        self.decided(self.confirm(doc, claim), "measure", None)
        self.result(9100, role="primary")
        # Attempt 2 of the upstream run, a pin-only retry, completes: same identity.
        self.github.upstream(attempts=2, conclusion="failure")
        self.github.published(attempt=2, publish=None, pin="failure", first_id=50)
        again, claim = self.handoff(9300, payload=self.github.payload(run_attempt=2))
        self.decided(again, "linked", None)
        self.assertIsNone(claim)
        self.assertEqual(again["first_claim"]["actions_run_id"], 9100)
        self.assertEqual(again["key"], doc["key"])

    def test_an_interrupted_claim_is_incomplete_and_never_measured_again(self):
        # Run 9100 claimed and then was cancelled before any result; the downstream run is
        # rerun as a whole (attempt 2 of the same Actions run) and a pin retry follows.
        doc, claim = self.claimed()
        for run_id, attempt in ((9100, 2), (9300, 1)):
            with self.subTest(run=run_id, attempt=attempt):
                again, written = self.handoff(run_id, attempt)
                self.decided(again, "incomplete", "claim_without_result")
                self.assertIsNone(written)
                self.assertIn("never measured again automatically", again["detail"])
        names = [item["name"] for item in self.github.artifacts]
        self.assertEqual(names, [doc["claim_artifact"]], "nothing more was claimed")
        # An expired result is incomplete too.
        self.result(9100, expired=True)
        again, _ = self.handoff(9300)
        self.decided(again, "incomplete", "result_expired")

    def test_expired_or_unavailable_history_runs_nothing(self):
        doc, claim = self.handoff()
        # An upstream run older than the claims' retention: its history may have expired.
        old = self.handoff(wall=NOW + (release.HISTORY_DAYS + 1) * 86400)[0]
        self.decided(old, "incomplete", "history_expired")
        self.assertEqual(old["history"]["state"], "not_read")
        # An expired trusted claim.
        item = self.write_claim(doc, claim, NOW_MS - 1000)
        item["expired"] = True
        expired, written = self.handoff(9300)
        self.decided(expired, "incomplete", "history_expired")
        self.assertIsNone(written)
        # A claim whose download answers that it is gone.
        item["expired"] = False
        self.github.overrides[API + "/repos/%s/actions/artifacts/%d/zip" % (
            REPOSITORY, item["id"])] = transport.Response(410, {}, b"")
        self.decided(self.handoff(9300)[0], "incomplete", "history_expired")
        del self.github.overrides[API + "/repos/%s/actions/artifacts/%d/zip" % (
            REPOSITORY, item["id"])]
        # A listing that fails, or holds more claims than are read, is unavailable.
        listing = API + "/repos/%s/actions/artifacts?name=%s&per_page=100&page=1" % (
            REPOSITORY, doc["claim_artifact"])
        self.github.overrides[listing] = transport.Response(503, {}, b"")
        unavailable, _ = self.handoff(9300)
        self.decided(unavailable, "incomplete", "history_unavailable")
        self.assertEqual(unavailable["history"]["state"], "unavailable")
        del self.github.overrides[listing]
        for index in range(release.CLAIMS_MAX):
            self.github.upload(doc["claim_artifact"], b"PK", 600 + index, branch="topic")
        unavailable, written = self.handoff(9300)
        self.decided(unavailable, "incomplete", "history_unavailable")
        self.assertIsNone(written)

    def test_a_claim_that_is_not_yet_listed_is_not_confirmed(self):
        doc, claim = self.handoff()
        confirmed = self.confirm(doc, claim)
        self.decided(confirmed, "incomplete", "claim_unconfirmed")

    # Explicit remeasurement -------------------------------------------------------------------

    def test_an_explicit_remeasurement_measures_anew_and_keeps_its_parent(self):
        doc, claim = self.claimed()
        self.decided(self.confirm(doc, claim), "measure", None)
        parent = [dict(item) for item in self.github.artifacts]
        again, remeasure = self.handoff(9400, remeasure=(UPSTREAM, doc["key"]))
        self.decided(again, "claimed", None)
        self.assertEqual(again["mode"], "remeasure")
        self.assertEqual(again["claim_artifact"], "af-release-remeasure-%s-9400"
                         % doc["key"][7:])
        self.assertEqual(remeasure["remeasure_of"], {
            "key": doc["key"], "claim_artifact_id": parent[0]["id"], "actions_run_id": 9100,
            "lab_run_id": "gh9100_a1_primary"})
        self.write_claim(again, remeasure)
        confirmed = self.confirm(again, remeasure)
        self.decided(confirmed, "measure", None)
        # The parent claim is untouched and still the release's first valid claim.
        self.assertEqual([dict(item) for item in self.github.artifacts[:1]], parent)
        self.result(9100, role="primary")
        later, _ = self.handoff(9500)
        self.decided(later, "linked", None)
        self.assertEqual(later["first_claim"]["actions_run_id"], 9100)

    def test_a_remeasurement_must_name_this_identity_and_an_existing_parent(self):
        doc, claim = self.handoff()
        self.decided(self.handoff(9400, remeasure=(UPSTREAM, doc["key"]))[0], "refused",
                     "parent_claim_unavailable")
        self.decided(self.handoff(9400, remeasure=(UPSTREAM, "sha256:" + "0" * 64))[0],
                     "refused", "remeasure_mismatch")
        self.decided(self.handoff(9400, remeasure=(UPSTREAM, "v0.2.0"))[0], "refused",
                     "invalid_dispatch")
        self.decided(self.handoff(9400, remeasure=(None, doc["key"]))[0], "refused",
                     "invalid_dispatch")

    def test_a_claim_artifact_holds_exactly_its_claim(self):
        _, claim = self.handoff()
        self.assertEqual(release.read_claim_zip(claim_zip(claim)), claim)
        for members in ([("claim.json", b"{}")],
                        [("claim.json", contracts.write_bytes(claim, "release-claim")),
                         ("run.sh", b"echo")],
                        [("../claim.json", contracts.write_bytes(claim, "release-claim"))]):
            with self.subTest(members=[name for name, _ in members]):
                with self.assertRaises(Exception):
                    release.read_claim_zip(claim_zip(None, members))


class Scheduling(test_planning.History):
    """A confirmed claim plans exactly its release comparison from the trusted control."""

    def claim(self, kind="release", run_id=9100, sha=None, tag="v0.2.0"):
        identity = {"repository": REPOSITORY, "upstream_run_id": UPSTREAM, "tag": tag,
                    "sha": sha or self.c2}
        key = release.identity_key(identity)
        parent = None
        if kind == "remeasure":
            parent = {"key": key, "claim_artifact_id": 8801, "actions_run_id": 9100,
                      "lab_run_id": "gh9100_a1_primary"}
        return contracts.validate_release_claim({
            "schema": contracts.RELEASE_CLAIM_SCHEMA, "kind": kind, "key": key,
            "identity": identity,
            "upstream": {"workflow_id": RELEASE_WORKFLOW_ID,
                         "workflow_path": release.RELEASE_WORKFLOW_PATH, "run_attempt": 1,
                         "published_in_attempt": 1},
            "release": {"id": 501, "prerelease": "-rc." in tag, "published_at": stamp(NOW_MS)},
            "claimant": {"repository": REPOSITORY,
                         "workflow_path": release.HANDOFF_WORKFLOW_PATH,
                         "actions_run_id": run_id, "actions_run_attempt": 1,
                         "lab_run_prefix": "gh%d_a1_" % run_id},
            "remeasure_of": parent, "claimed_unix_ms": NOW_MS})

    def schedule(self, claim, name="workflow_run", run_id=9100):
        event = ci.Event(name, {}, self.c3, "refs/heads/main", REPOSITORY, run_id, 1)
        settings = ci.Settings({"linux_gnu_native_release": IMAGE,
                                "linux_musl_shipping": MUSL})
        doc, plans = ci.schedule(self.git, event, settings, release=claim)
        self.assertEqual(contracts.read_json(contracts.write_bytes(doc, "schedule"),
                                             "schedule"), doc)
        return doc, plans

    def test_a_release_claim_compares_the_tag_with_the_highest_smaller_stable_release(self):
        doc, plans = self.schedule(self.claim())
        roles = dict((item["role"], item) for item in doc["comparisons"])
        self.assertEqual((roles["primary"]["status"], roles["primary"]["reason"]),
                         ("unavailable", "no_active_authority"))
        bootstrap = roles["control_bootstrap"]
        self.assertEqual(bootstrap["status"], "planned", bootstrap["detail"])
        plan = contracts.read_json(plans[bootstrap["run_id"]], "run")
        self.assertEqual(plan["trigger"], {"kind": "release", "tag": "v0.2.0",
                                           "tag_sha": self.c2, "baseline_tag": "v0.1.0",
                                           "prerelease": False})
        self.assertEqual((plan["resolved"]["baseline_sha"], plan["resolved"]["candidate_sha"],
                          plan["resolved"]["control_sha"]), (self.root, self.c2, self.c2))
        self.assertEqual(plan["tier"], "release")
        self.assertTrue(doc["trust"]["trusted"])
        self.assertEqual(doc["release"]["claim_artifact"],
                         "af-release-claim-" + doc["release"]["key"][7:])
        self.assertEqual((doc["enforcement"], doc["retention_days"]), ("advisory", 90))
        self.assertTrue(bootstrap["run_id"].startswith("gh9100_a1_"))

    def test_a_remeasurement_names_its_parent_run_and_the_same_two_commits(self):
        doc, plans = self.schedule(self.claim("remeasure", run_id=9400),
                                   name="workflow_dispatch", run_id=9400)
        bootstrap = dict((item["role"], item) for item in doc["comparisons"])[
            "control_bootstrap"]
        plan = contracts.read_json(plans[bootstrap["run_id"]], "run")
        self.assertEqual(plan["trigger"], {"kind": "manual",
                                           "remeasure_of": "gh9100_a1_primary"})
        self.assertEqual((plan["resolved"]["baseline_sha"], plan["resolved"]["candidate_sha"],
                          plan["resolved"]["control_sha"]), (self.root, self.c2, self.c2))
        self.assertEqual(doc["release"]["remeasure_of"], "gh9100_a1_primary")

    def test_a_moved_tag_or_another_runs_claim_measures_nothing(self):
        doc, plans = self.schedule(self.claim(sha=self.c1))
        self.assertEqual([(item["status"], item["reason"]) for item in doc["comparisons"]],
                         [("unavailable", "tag_moved")])
        self.assertEqual((plans, doc["matrix"]), ({}, []))
        with self.assertRaises(Exception):
            self.schedule(self.claim(run_id=9999))
        with self.assertRaises(Exception):
            self.schedule(self.claim(), name="push")

    def test_a_workflow_run_without_a_confirmed_claim_is_skipped(self):
        event = ci.Event("workflow_run", {}, self.c3, "refs/heads/main", REPOSITORY, 9100, 1)
        doc, plans = ci.schedule(self.git, event, ci.Settings({}))
        self.assertEqual((doc["status"], doc["reason"], plans), ("skipped", "unsupported_event",
                                                                  {}))
