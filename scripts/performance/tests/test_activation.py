"""Activation verification and equivalent-adapter admission (ADR-0183).

The cohorts are synthetic and labelled so: these tests establish what an activation must verify,
never that any policy is calibrated. The fetched path runs against a stand-in GitHub API that
answers from a synthetic cohort's retained evidence, as the documented endpoints shape it.
"""
import copy
import io
import json
import os
import shutil
import tempfile
import unittest
import zipfile
from contextlib import redirect_stderr, redirect_stdout
from unittest import mock
from urllib.parse import urlsplit

from fractions import Fraction

from perflab import activation, calibration, compare, contracts, equivalence, gitrefs
from perflab import planning, stats, suite, tomlfile, transport
from perflab.values import ContractError

from tests import activation_fixtures as records
from tests import calibration_fixtures as fixtures
from tests import support

API = "https://api.example.test"
STORAGE = "https://storage.example.test"


class Shared(object):
    cohort = tmp = invalid = invalid_originals = None


def setUpModule():
    Shared.cohort = fixtures.shared()
    Shared.tmp = tempfile.mkdtemp(prefix="af-perf-activation-")
    # A cohort whose power_01 and power_02 originals injected outside their dose band: 8 of 10.
    lab = Shared.cohort.prepared.clone(os.path.join(Shared.tmp, "invalid"))
    minute = 0
    for member in lab.cohort["members"]:
        if member["kind"] in ("injection", "power"):
            minute += 1
            dose = None
            if member["member_id"] in ("power_01", "power_02"):
                def dose(slot):
                    return fixtures.COST_NS * 200000 * 2 / 1000000
            lab.run(member["member_id"], minute=minute, dose=dose)
    Shared.invalid = lab


def tearDownModule():
    shutil.rmtree(Shared.tmp, True)


def ready_files(lab, doc=None):
    """The reviewed records of `lab`'s cohort, with `doc` as its readiness."""
    doc = doc or Shared.cohort.doc
    files = {records.COHORT_PATH: contracts.write_bytes(lab.cohort, "cohort"),
             records.CALIBRATION_PATH: contracts.write_bytes(lab.calibration,
                                                             "calibration-parameters"),
             records.READINESS_PATH: contracts.write_bytes(doc, "readiness")}
    return files, (lab.cohort, lab.calibration, doc)


def forge_ready(cohort, sealed, doc, members):
    """A self-consistent readiness document in which `members` detected with valid doses:
    every tally, qualification and decision recomputed, as an author could write it."""
    forged = copy.deepcopy(doc)
    for entry in forged["members"]:
        if entry["member_id"] in members:
            for case in entry["cases"].values():
                case.update(classification=stats.REGRESSION,
                            injected_median_ms=case["target_ms"], injection_valid=True)
    tallies, groups, qualifies, ready = contracts.compute_readiness(cohort, sealed,
                                                                    forged["members"])
    for entry in forged["members"]:
        entry["qualifies"] = qualifies[entry["member_id"]]
    forged.update(tallies=tallies, groups=groups, ready=ready)
    return contracts.validate_readiness(forged, cohort=cohort, calibration=sealed)


class FakeGitHub(object):
    """The documented REST endpoints the trusted fetcher reads, answered from a lab's evidence:
    workflow-run pages newest first, attempt records, attempt jobs, per-run artifact listings
    and artifact downloads redirected to signed storage."""

    def __init__(self, lab, page_size=7):
        self.lab = lab
        self.page_size = page_size
        self.requests = []

    def _json(self, doc):
        return transport.Response(200, {}, json.dumps(doc).encode())

    def request(self, url, headers, limit, timeout):
        self.requests.append(url)
        parts = urlsplit(url)
        query = dict(item.split("=") for item in parts.query.split("&") if item)
        page = int(query.get("page", "1"))
        if url.startswith(STORAGE):
            assert "Authorization" not in headers
            ident = int(parts.path.rsplit("/", 1)[1])
            item = [entry for entry in self.lab.artifacts if entry["id"] == ident][0]
            buffer = io.BytesIO()
            root = os.path.join(self.lab.evidence, item["name"])
            with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as container:
                for current, _, names in sorted(os.walk(root)):
                    for name in sorted(names):
                        path = os.path.join(current, name)
                        with open(path, "rb") as stream:
                            container.writestr(os.path.relpath(path, root), stream.read())
            return transport.Response(200, {}, buffer.getvalue())
        assert headers.get("Authorization") == "Bearer token"
        prefix = "/repos/%s" % fixtures.REPOSITORY
        path = parts.path[len(prefix):]
        if path == "/actions/workflows/performance.yml/runs":
            runs = sorted(self.lab.workflow_runs.values(), key=lambda run: -run["run_number"])
            chunk = runs[(page - 1) * self.page_size:page * self.page_size]
            return self._json({"total_count": len(runs), "workflow_runs": chunk})
        pieces = path.split("/")
        bridged = getattr(self.lab, "runs", None)
        if bridged is not None and len(pieces) >= 4 and pieces[2] == "runs":
            run = bridged.get(int(pieces[3]))
            if run is None:
                return transport.Response(404, {}, b"")
            if len(pieces) == 4:
                return self._json(run)
            if len(pieces) == 6 and pieces[4] == "attempts":
                return self._json(self.lab.attempt(int(pieces[3]), int(pieces[5])))
            if len(pieces) == 7 and pieces[6] == "jobs":
                jobs = self.lab.jobs.get((int(pieces[3]), int(pieces[5])), [])
                return self._json({"total_count": len(jobs),
                                   "jobs": jobs[(page - 1) * 100:page * 100]})
        if path.endswith("/attempts/1"):
            with open(self.lab.attempt_record(int(pieces[3]))) as stream:
                return self._json(json.load(stream))
        if path.endswith("/attempts/1/jobs"):
            with open(self.lab.jobs_record(int(pieces[3]))) as stream:
                return self._json(json.load(stream))
        if path.endswith("/artifacts") and pieces[2] == "runs":
            listed = [item for item in self.lab.artifacts
                      if item["workflow_run"]["id"] == int(pieces[3])]
            chunk = listed[(page - 1) * 100:page * 100]
            return self._json({"total_count": len(listed), "artifacts": chunk})
        if path.endswith("/zip"):
            return transport.Response(302, {"Location": "%s/blob/%s" % (STORAGE, pieces[3])},
                                      b"")
        return transport.Response(404, {}, b"")


class Verification(support.TempDir):
    def repo(self, files, manifest_bytes):
        """A copy of the authority repository with the reviewed records and manifest committed
        on top; (Git, the control revision)."""
        shutil.copytree(Shared.cohort.repo.path, self.path("repo"), symlinks=True)
        repo = support.Repo.existing(self.path("repo"))
        files = dict(files)
        files[planning.ACTIVATION_PATH] = manifest_bytes
        control = repo.commit("activate", files)
        return gitrefs.Git(repo.path), control

    def fetched(self, lab, git, control, **options):
        collector = transport.Collector(FakeGitHub(lab), fixtures.REPOSITORY, "token",
                                        api_url=API)
        manifest, _ = activation.read_manifest(git, control)
        cohort = contracts.read_json(git.read_file(control, manifest["cohort_path"]), "cohort")
        target = self.path("fetched-%d" % len(os.listdir(self.tmp)))
        evidence = activation.fetch_evidence(collector, cohort, os.path.join(target, "evidence"))
        os.makedirs(os.path.join(target, "runs"))
        with mock.patch.object(compare, "evaluate", fixtures.evaluate_once):
            return activation.verify(git, control, evidence=evidence,
                                     scratch=os.path.join(target, "runs"), replan=lab.replan,
                                     producer=lab.producer,
                                     **options)

    def test_a_ready_cohort_verifies_from_its_records_and_from_its_fetched_evidence(self):
        lab = Shared.cohort.lab
        files, documents = ready_files(lab)
        git, control = self.repo(files, records.manifest(files, documents))
        reviewed = activation.verify(git, control).doc
        self.assertEqual((reviewed["mode"], reviewed["admission"], reviewed["recomputed"]),
                         ("reviewed", "calibrated", None))
        self.assertEqual(reviewed["calibrated_groups"], ["hosted_x86"])
        self.assertEqual(reviewed["uncalibrated_groups"], {})
        self.assertEqual(reviewed["original_runs"], calibration.original_runs(Shared.cohort.doc))
        self.assertEqual(reviewed["pending"], ["owner_review_at_exact_head"])
        fetched = self.fetched(lab, git, control).doc
        self.assertEqual(fetched["mode"], "fetched")
        self.assertEqual(fetched["recomputed"], {
            "history_latest_run_number": Shared.cohort.doc["history"]["latest_run_number"],
            "refused_evidence": 0})
        self.assertEqual(dict(fetched, mode=None, recomputed=None),
                         dict(reviewed, mode=None, recomputed=None))
        contracts.read_json(contracts.write_bytes(fetched, "activation-verification"),
                            "activation-verification")
        # Runs created after readiness was written change neither the judgment nor the result.
        later = lab.clone(self.path("later"))
        later.other_run()
        self.assertEqual(self.fetched(later, git, control).doc["recomputed"][
            "history_latest_run_number"], fixtures.ANCHOR + 63)

    def test_author_written_success_contradicted_by_the_samples_never_activates(self):
        # power_01 and power_02 injected twice their dose: the honest readiness is 8 of 10. A
        # self-consistent document claiming valid regressions there passes every reviewed check;
        # only recomputation from the retained samples refuses it.
        lab = Shared.invalid
        honest = lab.readiness()
        self.assertEqual((honest["tallies"]["power_qualifying"], honest["ready"]), (8, False))
        forged = forge_ready(lab.cohort, lab.calibration, honest, ("power_01", "power_02"))
        self.assertTrue(forged["ready"])
        files, documents = ready_files(lab, forged)
        git, control = self.repo(files, records.manifest(files, documents))
        self.assertEqual(activation.verify(git, control).doc["mode"], "reviewed")
        with self.assertRaisesRegex(ContractError, "member power_01: complete with no reason, "
                                    "recorded complete") as caught:
            self.fetched(lab, git, control)
        self.assertIn("never activates", str(caught.exception))

    def test_missing_or_expired_members_refuse_fetched_activation(self):
        lab = Shared.cohort.lab.clone(self.path("expired"))
        files, documents = ready_files(lab)
        git, control = self.repo(files, records.manifest(files, documents))
        member = [item for item in Shared.cohort.doc["members"]
                  if item["member_id"] == "power_03"][0]
        lab.artifact(member["provenance"]["actions_run_id"])["expired"] = True
        with self.assertRaisesRegex(ContractError, "member power_03: missing with "
                                    "artifact_expired"):
            self.fetched(lab, git, control)
        gone = Shared.cohort.lab.clone(self.path("gone"))
        gone.delete_run(member["provenance"]["actions_run_id"])
        with self.assertRaisesRegex(ContractError, "not listed"):
            self.fetched(gone, git, control)

    def test_altered_records_wrong_scope_and_wrong_identities_are_refused(self):
        lab = Shared.cohort.lab
        files, documents = ready_files(lab)
        tampered = dict(files)
        tampered[records.READINESS_PATH] = files[records.READINESS_PATH].replace(
            b'"ready":true', b'"ready":true ')
        cases = (
            (files, {"readiness_sha256": records.sha256(b"other")}, "has SHA-256"),
            (files, {"cohort_sha256": records.sha256(b"other")}, "has SHA-256"),
            (files, {"calibration_sha256": records.sha256(b"other")}, "has SHA-256"),
            (files, {"environment_id": "linux_x86_64_musl_hosted"}, "never broadens"),
            (files, {"recipe_id": "linux_musl_shipping"}, "never broadens"),
            (files, {"calibrated_groups": ["elsewhere", "hosted_x86"]}, "calibrated_groups"),
            (files, {"cohort_actions_run_ids": [1]}, "cohort_actions_run_ids"),
            (files, {"cohort_id": "another"}, "cohort_id"),
            (files, {"policy_digest": "sha256:" + "5" * 64}, "policy"),
            (files, {"authority_sha": "0" * 40}, "the cohort calibrated authority"),
        )
        for index, (given, changes, why) in enumerate(cases):
            with self.subTest(why=why, changes=sorted(changes)):
                manifest = records.manifest(given, documents, **changes)
                shutil.rmtree(self.path("repo"), True)
                git, control = self.repo(given, manifest)
                with self.assertRaisesRegex(ContractError, why):
                    activation.verify(git, control)
        shutil.rmtree(self.path("repo"), True)
        git, control = self.repo(tampered, records.manifest(files, documents))
        with self.assertRaisesRegex(ContractError, "has SHA-256"):
            activation.verify(git, control)

    def test_an_unprepared_group_is_never_presented_as_calibrated(self):
        # Every observation ran in group a; declared group b has no A0 or loop count. The
        # cohort is ready as a whole, and only group a is calibrated.
        shutil.copytree(Shared.cohort.repo.path, self.path("repo"), symlinks=True)
        repo = support.Repo.existing(self.path("repo"))
        files, documents = records.records(
            suite.load_bytes(support.read(support.SYNTHETIC_SUITE)), Shared.cohort.authority,
            groups=(("a", "CPU A", "image-a"), ("b", "CPU B", "image-b")), prepared=["a"])
        cohort, sealed, ready = documents
        self.assertTrue(ready["ready"])
        self.assertEqual(ready["groups"]["b"]["ready"], False)
        for groups, why in ((["a", "b"], "only in a; b lacks"), (["b"], "only in a")):
            with self.subTest(groups=groups):
                files[planning.ACTIVATION_PATH] = records.manifest(files, documents,
                                                                   calibrated_groups=groups)
                control = repo.commit("activate %s" % groups, files)
                with self.assertRaisesRegex(ContractError, why):
                    activation.verify(gitrefs.Git(repo.path), control)
        files[planning.ACTIVATION_PATH] = records.manifest(files, documents)
        control = repo.commit("activate a", files)
        doc = activation.verify(gitrefs.Git(repo.path), control).doc
        self.assertEqual(doc["calibrated_groups"], ["a"])
        critical = sorted(case["instance_id"] for case in cohort["cases"]
                          if case["criticality"] == "critical")
        self.assertEqual(doc["uncalibrated_groups"], {"b": critical})
        self.assertIn("Not calibrated: group b", activation.summary_markdown(doc))

    def test_the_cli_verifies_reviewed_and_fetched_activations(self):
        import cli
        lab = Shared.cohort.lab
        files, documents = ready_files(lab)
        git, control = self.repo(files, records.manifest(files, documents))
        output = self.path("verification.json")
        summary = self.path("summary.md")
        with redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
            code = cli.main(["activate", "--verify", "--repo", git.repo, "--output", output,
                             "--job-summary", summary])
        self.assertEqual(code, 0)
        self.assertEqual(contracts.read_file(output, "activation-verification")["mode"],
                         "reviewed")
        base = git.resolve(control + "^", "base")
        # The proposal adds the manifest and every record it names: all are fetched evidence.
        self.assertEqual(activation.proposed_changes(git, base, control),
                         sorted([planning.ACTIVATION_PATH] + list(files)))
        fake = FakeGitHub(lab)
        errors = io.StringIO()
        with mock.patch.object(transport, "HttpsTransport", lambda: fake), \
                mock.patch.object(calibration, "replanner", lambda *args: lab.replan), \
                mock.patch.object(calibration, "producer_check", lambda *args: lab.producer), \
                mock.patch.object(compare, "evaluate", fixtures.evaluate_once), \
                mock.patch.dict(os.environ, {"AF_PERF_TOKEN": "token"}), \
                redirect_stderr(errors), redirect_stdout(io.StringIO()):
            code = cli.main(["activate", "--verify-proposed", "--repo", git.repo,
                             "--base", base, "--revision", control,
                             "--repository", fixtures.REPOSITORY, "--token-env",
                             "AF_PERF_TOKEN", "--api-url", API,
                             "--output", self.path("fetched.json")])
        self.assertEqual(code, 0, errors.getvalue())
        self.assertEqual(contracts.read_file(self.path("fetched.json"),
                                             "activation-verification")["mode"], "fetched")
        self.assertTrue(any("/attempts/1/jobs" in url for url in fake.requests))
        # An ordinary pull request after it leaves the activation unchanged: it is verified
        # locally, the old cohort is not fetched again, and no token is needed.
        later = support.Repo.existing(git.repo).commit("ordinary", {"README": b"later\n"})
        self.assertEqual(activation.proposed_changes(git, control, later), [])
        quiet = FakeGitHub(lab)
        errors = io.StringIO()
        with mock.patch.object(transport, "HttpsTransport", lambda: quiet), \
                mock.patch.dict(os.environ, {}, clear=True), \
                redirect_stderr(errors), redirect_stdout(io.StringIO()):
            code = cli.main(["activate", "--verify-proposed", "--repo", git.repo,
                             "--base", control, "--revision", later,
                             "--output", self.path("unchanged.json")])
        self.assertEqual(code, 0, errors.getvalue())
        self.assertEqual(contracts.read_file(self.path("unchanged.json"),
                                             "activation-verification")["mode"], "reviewed")
        self.assertEqual(quiet.requests, [])
        self.assertIn("unchanged", errors.getvalue())
        # A changed record is a changed activation; without a repository it is refused.
        retouched = support.Repo.existing(git.repo).commit("retouch", {
            records.READINESS_PATH: files[records.READINESS_PATH] + b"\n"})
        self.assertEqual(activation.proposed_changes(git, later, retouched),
                         [records.READINESS_PATH])
        with redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
            self.assertEqual(cli.main(["activate", "--verify-proposed", "--repo", git.repo,
                                       "--base", later, "--revision", retouched]), 2)
            self.assertEqual(cli.main(["activate", "--verify-proposed", "--repo", git.repo,
                                       "--revision", later]), 2)
        # The command only verifies: without --verify it refuses and writes nothing.
        with redirect_stderr(io.StringIO()):
            self.assertEqual(cli.main(["activate", "--repo", git.repo]), 2)


class CappedListing(FakeGitHub):
    """The stand-in API with one original's artifact or job listing replaced: `pages` maps a
    page number to the listing that page answers."""

    def __init__(self, lab, run_id, kind, pages):
        FakeGitHub.__init__(self, lab)
        self.run_id, self.kind, self.pages = run_id, kind, pages

    def request(self, url, headers, limit, timeout):
        parts = urlsplit(url)
        suffix = {"artifacts": "/actions/runs/%d/artifacts" % self.run_id,
                  "jobs": "/actions/runs/%d/attempts/1/jobs" % self.run_id}[self.kind]
        if parts.path.endswith(suffix):
            self.requests.append(url)
            query = dict(item.split("=") for item in parts.query.split("&") if item)
            return self._json(self.pages(int(query["page"])))
        return FakeGitHub.request(self, url, headers, limit, timeout)


class Listings(support.TempDir):
    """A capped, inconsistent or doubled listing is refused before it is evidence (ADR-0184)."""

    def fetch(self, fake):
        lab = Shared.cohort.lab
        collector = transport.Collector(fake, fixtures.REPOSITORY, "token", api_url=API)
        target = self.path("fetched-%d" % len(os.listdir(self.tmp)))
        return activation.fetch_evidence(collector, lab.cohort, target)

    def test_a_listing_longer_than_its_page_bound_is_refused_never_shortened(self):
        lab = Shared.cohort.lab
        run_id = Shared.cohort.doc["members"][0]["provenance"]["actions_run_id"]
        own = [item for item in lab.artifacts if item["workflow_run"]["id"] == run_id]
        # The predecessor read ten pages of a 1001-artifact listing, wrote total_count 1000 and
        # never saw the duplicate of the expected bundle on page 11.
        filler = [dict(own[-1], id=500000 + index, name="unrelated-%d" % index)
                  for index in range(1000 - len(own))]
        listing = own + filler + [dict(own[0], id=900000)]

        def pages(page):
            return {"total_count": 1001,
                    "artifacts": listing[(page - 1) * 100:page * 100]}
        fake = CappedListing(lab, run_id, "artifacts", pages)
        with self.assertRaisesRegex(ContractError, "holds 1000 of its 1001 artifacts within "
                                    "10 pages"):
            self.fetch(fake)
        self.assertEqual(sum(1 for url in fake.requests
                             if "/runs/%d/artifacts" % run_id in url), 10)
        for why, answer in (
                ("lists artifacts %d twice" % own[0]["id"],
                 lambda page: {"total_count": len(own) + 1, "artifacts": own + own[:1]}),
                ("changed its total_count",
                 lambda page: {"total_count": len(own) + page - 1,
                               "artifacts": own[:1] if page == 1 else own[1:]}),
                ("holds %d of its %d" % (len(own), len(own) + 1),
                 lambda page: {"total_count": len(own) + 1,
                               "artifacts": own if page == 1 else []}),
                ("has no artifacts and total_count", lambda page: {"artifacts": own})):
            with self.subTest(why=why):
                with self.assertRaisesRegex(ContractError, why):
                    self.fetch(CappedListing(lab, run_id, "artifacts", answer))

    def test_an_incomplete_or_doubled_job_listing_is_refused(self):
        lab = Shared.cohort.lab
        run_id = Shared.cohort.doc["members"][0]["provenance"]["actions_run_id"]
        with open(lab.jobs_record(run_id)) as stream:
            jobs = json.load(stream)["jobs"]
        for why, answer in (
                ("holds %d of its %d jobs" % (len(jobs) - 1, len(jobs)),
                 lambda page: {"total_count": len(jobs),
                               "jobs": jobs[:-1] if page == 1 else []}),
                ("lists jobs %d twice" % jobs[0]["id"],
                 lambda page: {"total_count": len(jobs), "jobs": jobs[:1] + jobs[:-1]})):
            with self.subTest(why=why):
                with self.assertRaisesRegex(ContractError, why):
                    self.fetch(CappedListing(lab, run_id, "jobs", answer))
        # A complete listing is retained whole, its count the observed one.
        evidence = self.fetch(FakeGitHub(lab))
        with open(os.path.join(evidence, calibration.ACTIONS_FILE)) as stream:
            listing = json.load(stream)
        self.assertEqual(listing["total_count"], len(listing["artifacts"]))

    def test_the_reader_refuses_a_listing_shorter_than_its_count_or_doubled(self):
        lab = Shared.cohort.lab.clone(self.path("reader"))
        lab.write_listing()
        path = os.path.join(lab.evidence, calibration.ACTIONS_FILE)
        with open(path) as stream:
            listing = json.load(stream)
        for why, doc in (
                ("lists %d of %d artifacts" % (len(listing["artifacts"]) - 1,
                                              len(listing["artifacts"])),
                 dict(listing, artifacts=listing["artifacts"][:-1])),
                ("is listed twice", dict(listing, total_count=len(listing["artifacts"]) + 1,
                                         artifacts=listing["artifacts"]
                                         + listing["artifacts"][:1]))):
            with self.subTest(why=why):
                with open(path, "w") as stream:
                    json.dump(doc, stream)
                with self.assertRaisesRegex(ContractError, why):
                    calibration.collect(lab.cohort, lab.evidence, self.path("s"), lab.replan,
                                        lab.producer)


class Versions(support.TempDir):
    def test_an_earlier_activation_manifest_is_refused_and_never_read(self):
        # af.perf-activation/1 bound only readiness bytes nobody rehashed (ADR-0183).
        shutil.copytree(Shared.cohort.repo.path, self.path("repo"), symlinks=True)
        repo = support.Repo.existing(self.path("repo"))
        earlier = ('schema = "af.perf-activation/1"\nauthority_sha = "%s"\n'
                   'policy_digest = "%s"\nreadiness_sha256 = "sha256:%s"\n'
                   'cohort_id = "first_cohort"\ncohort_actions_run_ids = [101]\n\n'
                   '[admission]\nkind = "calibrated"\n'
                   % (Shared.cohort.authority, Shared.cohort.lab.cohort["policy_digest"],
                      "ab" * 32))
        control = repo.commit("earlier", {planning.ACTIVATION_PATH: earlier})
        git = gitrefs.Git(repo.path)
        with self.assertRaisesRegex(ContractError, "expected af.perf-activation/3"):
            activation.read_manifest(git, control)
        # A push after it, whose control is that revision, plans nothing from it.
        later = repo.commit("later", {"README": "later\n"})
        with self.assertRaisesRegex(ContractError, "expected af.perf-activation/3"):
            planning.plan(git, planning.Request(trigger="main_push", tier="quick",
                                                scope="full", head=later, run_id="r",
                                                trusted_host_build=True))


class EquivalentAdapter(support.TempDir):
    """A bridge may change only its named adapter files and registry support entries, and
    measures as active only with verified execution evidence (ADR-0183, ADR-0184)."""

    GROUP = records.BRIDGE_ADAPTER
    CLOSURE = "crates/review-pipeline/examples/perf-probe/cases.rs"
    RETAINED = "fixtures/performance/activations/first.toml"

    def calibrated(self, name):
        """A repository whose calibrated authority holds the adapter registry and its files,
        with that authority's reviewed records; (repo, records, documents)."""
        shutil.copytree(Shared.cohort.repo.path, self.path(name), symlinks=True)
        repo = support.Repo.existing(self.path(name))
        adapters = support.read(os.path.join(support.ROOT,
                                             "fixtures/performance/adapters.toml"))
        # The reviewed Performance workflow a bridge run's producer is checked against.
        authority = repo.commit("adapters", {"fixtures/performance/adapters.toml": adapters,
                                             self.CLOSURE: b"// v1\n",
                                             fixtures.WORKFLOW: b"name: Performance\n"})
        files, documents = records.records(
            suite.load_bytes(support.read(support.SYNTHETIC_SUITE)), authority)
        return repo, files, documents

    def evidence(self, name, git, documents, base_manifest, bridge, merged, golden=None,
                 aa=None):
        """The bridge's evidence record from a golden-check run (calibrated source against the
        bridge) and a post-merge A/A run of `merged`, both measured by the bridge; `golden` and
        `aa` change how each run is written. (record, {role: (run directory, Actions run)})."""
        cohort = documents[0]
        admission = {"equivalence_group": self.GROUP, "adapter_files": [self.CLOSURE]}
        ctx = equivalence.context(cohort, tomlfile.loads(base_manifest, "base"), admission,
                                  bridge, activation._adapters(git, bridge))
        runs = {}
        for role, (baseline, candidate, actions_run, options) in (
                ("golden", (cohort["authority_sha"], bridge, 8101, golden or {})),
                ("post_merge_aa", (merged, merged, 8102, aa or {}))):
            options = dict(options)
            plan = records.bridge_plan(git, options.pop("baseline", baseline),
                                       options.pop("candidate", candidate),
                                       options.pop("authority", bridge), actions_run,
                                       mode=options.pop("mode", "proposed"),
                                       edit=options.pop("plan_edit", None))
            run_dir = self.path("%s-%s" % (name, role))
            with mock.patch.object(compare, "evaluate", fixtures.evaluate_once):
                records.measured_run(plan, run_dir, **options)
            runs[role] = (run_dir, actions_run)
        with mock.patch.object(compare, "evaluate", fixtures.evaluate_once):
            return equivalence.compute(ctx, runs), runs

    def admit(self, name, bridge_files, adapter_files=None, golden=None, aa=None, edit=None):
        """Commit a bridge authority after the calibrated one and a post-merge descendant, then
        the records, the retained calibrated manifest, the bridge's evidence record and an
        equivalent-adapter manifest; verify the latter. (documents, verification, record,
        runs, Git, control)."""
        repo, files, documents = self.calibrated(name)
        calibrated_manifest = records.manifest(files, documents)
        bridge = repo.commit("bridge", bridge_files)
        merged = repo.commit("merged", {"README": b"after the bridge merged\n"})
        git = gitrefs.Git(repo.path)
        record, runs = self.evidence(name, git, documents, calibrated_manifest, bridge, merged,
                                     golden, aa)
        if edit is not None:
            record = edit(record)
        files = dict(files)
        files[self.RETAINED] = calibrated_manifest
        files[records.EQUIVALENCE_PATH] = contracts.document_bytes(record)
        files[planning.ACTIVATION_PATH] = records.manifest(
            files, documents, authority_sha=bridge,
            admission={"kind": "equivalent_adapter", "equivalence_group": self.GROUP,
                       "adapter_files": adapter_files or [self.CLOSURE],
                       "calibrated_activation_path": self.RETAINED,
                       "calibrated_activation_sha256": records.sha256(calibrated_manifest),
                       "equivalence_path": records.EQUIVALENCE_PATH,
                       "equivalence_sha256": records.sha256(
                           files[records.EQUIVALENCE_PATH]),
                       "rationale": "The same workload through a renamed API."})
        control = repo.commit("admit", files)
        return documents, activation.verify(git, control).doc, record, runs, git, control

    def test_a_bridge_with_verified_executions_is_admitted_with_owner_approval_pending(self):
        documents, doc, record, runs, git, control = self.admit(
            "bridge", {self.CLOSURE: b"// v2: the renamed API\n"})
        self.assertTrue(record["verified"])
        self.assertEqual(doc["admission"], "equivalent_adapter")
        self.assertEqual(doc["equivalence"]["changed_paths"], [self.CLOSURE])
        self.assertEqual(doc["equivalence"]["calibrated_authority_sha"],
                         documents[0]["authority_sha"])
        self.assertEqual(doc["equivalence"]["golden"]["baseline_sha"],
                         documents[0]["authority_sha"])
        self.assertEqual(doc["equivalence"]["golden"]["candidate_sha"], doc["authority_sha"])
        self.assertEqual(doc["equivalence"]["golden"]["lab_run_id"], "gh8101_a1_primary")
        # The executions are verified evidence; only the owner's semantic approval stays.
        self.assertEqual(doc["pending"], ["owner_review_at_exact_head",
                                          "semantic_equivalence_owner_approval"])
        self.assertIn("Verified golden", activation.summary_markdown(doc))
        # A full plan under the bridge is active and eligible.
        later = support.Repo.existing(git.repo).commit("candidate", {"README": b"later\n"})
        plan = planning.plan(git, planning.Request(
            trigger="main_push", tier="quick", scope="full", head=later, run_id="r",
            trusted_host_build=True))
        self.assertEqual((plan["authority"]["activation"]["admission"],
                          plan["required_check_eligible"]), ("equivalent_adapter", True))
        # Fetched verification recomputes the record from the retained runs.
        lab = Bridge(self.path("bridge-evidence"))
        for role in equivalence.ROLES:
            lab.stage(*runs[role])
        target = self.path("bridge-fetched")
        collector = transport.Collector(FakeGitHub(lab), fixtures.REPOSITORY, "token",
                                        api_url=API)
        activation.fetch_bridge_runs(collector, record, target)
        os.makedirs(self.path("bridge-scratch"))
        with mock.patch.object(compare, "evaluate", fixtures.evaluate_once):
            bound = activation._recompute_equivalence(
                record, target, self.path("bridge-scratch"), "activation.toml", git,
                fixtures.REPOSITORY, doc["authority_sha"])
        # Each run is bound to the Performance workflow, the attempt its lab run ID selects and
        # that attempt's jobs on hosted runners (ADR-0185).
        for role in equivalence.ROLES:
            self.assertEqual(bound[role]["workflow_path"], fixtures.WORKFLOW)
            self.assertEqual(bound[role]["run_attempt"], 1)
            shards = contracts.read_file(os.path.join(runs[role][0], "run.json"),
                                         "run")["shards"]
            self.assertEqual(bound[role]["runner"]["jobs"], 1 + len(shards))
        fetched = copy.deepcopy(doc)
        fetched["mode"] = "fetched"
        fetched["recomputed"] = {"history_latest_run_number": 1, "refused_evidence": 0}
        for role in equivalence.ROLES:
            fetched["equivalence"][role]["provenance"] = bound[role]
        contracts.validate_activation_verification(fetched)
        fetched["equivalence"]["golden"]["provenance"] = None
        with self.assertRaisesRegex(ContractError, "exactly a fetched verification binds"):
            contracts.validate_activation_verification(fetched)

    def test_bridge_runs_without_their_trusted_producer_are_refused(self):
        # The predecessor recomputed a bridge's record from artifact-only responses: any workflow
        # of the repository could upload measured-looking bundles under the lab run ID.
        documents, doc, record, runs, git, control = self.admit(
            "producer", {self.CLOSURE: b"// v2: the renamed API\n"})
        # A head commit whose workflow file another author changed: the run was not produced by
        # the reviewed Performance workflow, whatever it uploaded.
        changed_workflow = support.Repo.existing(git.repo).commit(
            "workflow", {fixtures.WORKFLOW: b"name: Performance\non: push\n"})
        cases = (
            ("workflow", {"golden": {"path": ".github/workflows/other.yml"}},
             "not the reviewed .github/workflows/performance.yml"),
            ("repository", {"golden": {"repository": {"full_name": "someone/else"}}},
             "ran in someone/else"),
            ("unsuccessful", {"attempt": {"conclusion": "failure"}},
             "only a completed successful attempt"),
            ("head", {"golden": {"head_sha": changed_workflow}},
             "the workflow at head commit .* differs from the bridged authority"),
            ("self_hosted", {"jobs": {"runner_group_name": "Default"}},
             "not all shown on fresh GitHub-hosted runners"))
        for name, change, why in cases:
            with self.subTest(case=name):
                lab = Bridge(self.path("producer-%s-evidence" % name))
                for role in equivalence.ROLES:
                    lab.stage(*runs[role], **change.get(role, {}))
                if "attempt" in change:
                    lab.attempts[(runs["golden"][1], 1)] = change["attempt"]
                if "jobs" in change:
                    for job in lab.jobs[(runs["golden"][1], 1)]:
                        job.update(change["jobs"])
                target = self.path("producer-%s-fetched" % name)
                collector = transport.Collector(FakeGitHub(lab), fixtures.REPOSITORY, "token",
                                                api_url=API)
                activation.fetch_bridge_runs(collector, record, target)
                os.makedirs(self.path("producer-%s-scratch" % name))
                with self.assertRaisesRegex(ContractError, why):
                    with mock.patch.object(compare, "evaluate", fixtures.evaluate_once):
                        activation._recompute_equivalence(
                            record, target, self.path("producer-%s-scratch" % name), "a", git,
                            fixtures.REPOSITORY, doc["authority_sha"])
        # A lab run ID of another Actions run, or of an attempt the record does not select, is
        # refused before anything is fetched.
        with self.assertRaisesRegex(ContractError, "was not planned by Actions run"):
            activation.lab_attempt("gh8101_a1_primary", 9999, "golden")
        self.assertEqual(activation.lab_attempt("gh8101_a2_primary", 8101, "golden"), 2)

    def fetched(self, name, record, runs, git, doc, **change):
        """Stage `runs` as the Performance workflow produced them, fetch them and recompute the
        bridge's record from them; `change`, {role: run record fields}, alters a producer."""
        lab = Bridge(self.path("%s-evidence" % name))
        for role in equivalence.ROLES:
            lab.stage(*runs[role], **change.get(role, {}))
        target = self.path("%s-fetched" % name)
        collector = transport.Collector(FakeGitHub(lab), fixtures.REPOSITORY, "token",
                                        api_url=API)
        activation.fetch_bridge_runs(collector, record, target)
        os.makedirs(self.path("%s-scratch" % name))
        with mock.patch.object(compare, "evaluate", fixtures.evaluate_once):
            return activation._recompute_equivalence(
                record, target, self.path("%s-scratch" % name), "a", git, fixtures.REPOSITORY,
                doc["authority_sha"])

    def test_a_bridge_run_the_bridged_authority_does_not_plan_again_is_refused(self):
        # The predecessor bound each run's producer but never its plan: a genuine plan frozen
        # from Git, then only the post-merge A/A's absolute tolerances widened from 1 to 1000
        # and its digests recomputed, made a candidate at 30 against a baseline at 10 verify.
        regression = lambda slot, case: Fraction(30 if slot["side"] == "candidate" else 10)

        def widen(plan):
            for case in plan["cases"]:
                case["tolerance"]["absolute"] = "1000"
        documents, doc, record, runs, git, control = self.admit(
            "tolerance", {self.CLOSURE: b"// v2\n"}, aa={"value": regression,
                                                          "plan_edit": widen})
        self.assertTrue(record["verified"], record["post_merge_aa"]["reasons"])
        with self.assertRaisesRegex(ContractError, "post_merge_aa run 8102: its frozen plan is "
                                    "not the plan the bridged authority freezes for this "
                                    "dispatch \\(cases differ\\)"):
            self.fetched("tolerance", record, runs, git, doc)
        # Under the plan the bridged authority freezes, the same values are a regression.
        with self.assertRaisesRegex(ContractError, "not within tolerance"):
            self.admit_failing("frozen-tolerance", {self.CLOSURE: b"// v2\n"}, None,
                               {"value": regression})
        # Every other frozen field is bound the same way, each edit self-consistent so that only
        # the bridged authority's own plan shows it: the golden check's environment, the
        # post-merge A/A's instrument and fixture digests.
        def runner_label(plan):
            profile = plan["profile"]
            profile["environment"]["runner_label"] = "a-faster-runner"
            profile["environment_digest"] = planning.canonical_digest(
                {"id": profile["environment_id"], "environment": profile["environment"]})
        for name, role, edit, why in (
                ("environment", "golden", runner_label, "profile"),
                ("instrument", "post_merge_aa", lambda plan: plan["authority"].update(
                    instrument_digest="sha256:" + "2" * 64), "authority"),
                ("fixtures", "post_merge_aa", lambda plan: plan["authority"].update(
                    fixture_digest="sha256:" + "3" * 64), "authority")):
            with self.subTest(changed=name):
                options = {role: {"plan_edit": edit}}
                documents, doc, record, runs, git, control = self.admit(
                    "changed-" + name, {self.CLOSURE: b"// v2\n"},
                    golden=options.get("golden"), aa=options.get("post_merge_aa"))
                self.assertTrue(record["verified"])
                with self.assertRaisesRegex(ContractError, "%s run .*: its frozen plan is not "
                                            "the plan .*\\(%s differ" % (role, why)):
                    self.fetched("changed-" + name, record, runs, git, doc)

    def test_a_bridge_run_with_another_control_and_the_same_producer_bytes_is_admitted(self):
        # What a dispatch chooses stays free: the A/A's control revision is a later commit
        # whose workflow and scheduling code are byte-identical to the bridge's.
        repo, files, documents = self.calibrated("control")
        calibrated_manifest = records.manifest(files, documents)
        bridge = repo.commit("bridge", {self.CLOSURE: b"// v2\n"})
        merged = repo.commit("merged", {"README": b"merged\n"})
        later = repo.commit("later control", {"NOTES": b"unrelated\n"})
        git = gitrefs.Git(repo.path)
        cohort = documents[0]
        ctx = equivalence.context(cohort, tomlfile.loads(calibrated_manifest, "base"),
                                  {"equivalence_group": self.GROUP,
                                   "adapter_files": [self.CLOSURE]},
                                  bridge, activation._adapters(git, bridge))
        replan = activation.bridge_replanner(git, ctx)
        golden = records.bridge_plan(git, cohort["authority_sha"], bridge, bridge, 8101)
        aa = records.bridge_plan(git, merged, merged, bridge, 8102, control=later)
        self.assertEqual(aa["resolved"]["control_sha"], later)
        for role, plan in (("golden", golden), ("post_merge_aa", aa)):
            activation._replanned_bridge(replan, role, plan, role)
        # A golden check of other sources is not the bridge's dispatch.
        other = records.bridge_plan(git, merged, bridge, bridge, 8101)
        with self.assertRaisesRegex(ContractError, "golden: its frozen plan is not"):
            activation._replanned_bridge(replan, "golden", other, "golden")

    def test_a_bridge_without_verified_executions_stays_unavailable(self):
        # The predecessor admitted a bridge naming only its protected path, its golden checks
        # and post-merge A/A listed as pending, and its full plan satisfied the required check.
        bridge_files = {self.CLOSURE: b"// v2\n"}
        cases = (
            ("oracle", {"golden": {"oracle_failure": "synthetic_fast"}},
             "golden: case synthetic_fast has"),
            ("old_adapter", {"golden": {"adapters": ("old_adapter", "old_adapter")}},
             "not the bridge's"),
            ("same_sources", {"golden": {"baseline": "bridge"}}, "not the calibrated product"),
            ("uncalibrated_host", {"aa": {"cpu": "Intel Xeon"}},
             "outside the calibrated CPU/image groups"),
            ("regression", {"aa": {"value": lambda slot, case: Fraction(
                30 if slot["side"] == "candidate" else 10)}}, "not within tolerance"),
            ("control_authority", {"aa": {"mode": "control"}}, "not the proposed bridge"))
        for name, options, why in cases:
            with self.subTest(why=name):
                golden = dict(options.get("golden", {}))
                if golden.get("baseline") == "bridge":
                    golden["baseline"] = None
                with self.assertRaisesRegex(ContractError, why):
                    self.admit_failing(name, bridge_files, golden, options.get("aa"))

    def admit_failing(self, name, bridge_files, golden, aa):
        repo, files, documents = self.calibrated(name)
        calibrated_manifest = records.manifest(files, documents)
        bridge = repo.commit("bridge", bridge_files)
        merged = repo.commit("merged", {"README": b"merged\n"})
        git = gitrefs.Git(repo.path)
        if golden and "baseline" in golden and golden["baseline"] is None:
            golden["baseline"] = bridge
        record, _ = self.evidence(name, git, documents, calibrated_manifest, bridge, merged,
                                  golden, aa)
        self.assertFalse(record["verified"])
        files = dict(files)
        files[self.RETAINED] = calibrated_manifest
        files[records.EQUIVALENCE_PATH] = contracts.document_bytes(record)
        files[planning.ACTIVATION_PATH] = self.bridge_manifest(files, documents, bridge,
                                                               calibrated_manifest)
        control = repo.commit("admit", files)
        try:
            activation.verify(git, control)
        except ContractError as error:
            self.assertIn("stays advisory", str(error))
            raise ContractError("; ".join(
                "%s: %s" % (role, reason) for role in equivalence.ROLES
                for reason in record[role]["reasons"]))

    def bridge_manifest(self, files, documents, bridge, calibrated_manifest, **admission):
        fields = {"kind": "equivalent_adapter", "equivalence_group": self.GROUP,
                  "adapter_files": [self.CLOSURE],
                  "calibrated_activation_path": self.RETAINED,
                  "calibrated_activation_sha256": records.sha256(calibrated_manifest),
                  "equivalence_path": records.EQUIVALENCE_PATH,
                  "equivalence_sha256": records.sha256(files[records.EQUIVALENCE_PATH]),
                  "rationale": "The same workload through a renamed API."}
        fields.update(admission)
        return records.manifest(files, documents, authority_sha=bridge, admission=fields)

    def test_forged_or_unbound_evidence_records_are_refused(self):
        def forge(record):
            # Self-consistent but naming another golden run's lab ID: reviewed checks pass the
            # shape and reasons; the context binding and recomputation catch the rest.
            forged = copy.deepcopy(record)
            forged["bridge_adapters"] = ["another_adapter"]
            return forged
        with self.assertRaisesRegex(ContractError, "reasons: do not follow|bridge_adapters"):
            self.admit("forged", {self.CLOSURE: b"// v2\n"}, edit=forge)

        def unmerged(record):
            forged = copy.deepcopy(record)
            for key in ("baseline_sha", "candidate_sha"):
                forged["post_merge_aa"][key] = record["calibrated_authority_sha"]
            return forged
        with self.assertRaisesRegex(ContractError, "does not descend from the bridge"):
            self.admit("unmerged", {self.CLOSURE: b"// v2\n"}, edit=unmerged)
        # The fetched recomputation refuses a record whose facts the runs do not show.
        documents, doc, record, runs, git, control = self.admit(
            "recompute", {self.CLOSURE: b"// v2\n"})
        forged = copy.deepcopy(record)
        forged["golden"]["verdict_sha256"] = "sha256:" + "1" * 64
        lab = Bridge(self.path("recompute-evidence"))
        for role in equivalence.ROLES:
            lab.stage(*runs[role])
        target = self.path("recompute-fetched")
        collector = transport.Collector(FakeGitHub(lab), fixtures.REPOSITORY, "token",
                                        api_url=API)
        activation.fetch_bridge_runs(collector, forged, target)
        os.makedirs(self.path("recompute-scratch"))
        with self.assertRaisesRegex(ContractError, "differs from the reviewed record "
                                    "\\(golden\\)"):
            with mock.patch.object(compare, "evaluate", fixtures.evaluate_once):
                activation._recompute_equivalence(forged, target,
                                                  self.path("recompute-scratch"), "a", git,
                                                  fixtures.REPOSITORY, doc["authority_sha"])
        # An expired run bundle refuses the fetch.
        lab.artifacts[0]["expired"] = True
        with self.assertRaisesRegex(ContractError, "has expired"):
            activation.fetch_bridge_runs(collector, record, self.path("expired"))

    def test_a_changed_registry_harness_build_input_or_other_adapter_needs_a_new_cohort(self):
        suite_bytes = support.read(support.SYNTHETIC_SUITE)
        adapters = support.read(os.path.join(support.ROOT,
                                             "fixtures/performance/adapters.toml"))
        cases = (
            ({"fixtures/performance/suite.toml": suite_bytes + b"\n# timing changed\n"},
             "fixtures/performance/suite.toml"),
            ({"scripts/performance/cli.py": b"# another instrument\n"},
             "scripts/performance/cli.py"),
            ({"crates/review-pipeline/examples/perf-probe/fixtures.rs": b"// workload\n"},
             "perf-probe/fixtures.rs"),
            ({"fixtures/performance/adapters.toml": adapters.replace(
                b'equivalence_group = "native_probe_public_api_v1"',
                b'equivalence_group = "native_probe_public_api_v2"', 1)},
             "outside equivalence group"),
            # The predecessor's protected diff omitted the instrument helpers: a bridge that
            # changed the calibration loop's multiplier beside its adapter passed (ADR-0185).
            ({"crates/review-core/src/perf_calibration.rs": b"// loop multiplier 3\n"},
             "crates/review-core/src/perf_calibration.rs"),
            ({"crates/review-core/src/perf_trace.rs": b"// another recorder\n"},
             "crates/review-core/src/perf_trace.rs"),
            # The predecessor's protected diff omitted the build closure (ADR-0184).
            ({"Cargo.lock": b"# lock stand-in\nversion = 4\n# changed\n"}, "Cargo.lock"),
            ({"crates/review-pipeline/Cargo.toml": b"[package]\n"},
             "crates/review-pipeline/Cargo.toml"),
            ({"crates/af/build.rs": b"fn main() {}\n"}, "crates/af/build.rs"),
            ({"rust-toolchain.toml": b"[toolchain]\nchannel = \"1.89.0\"\n"},
             "rust-toolchain.toml"),
            ({".cargo/config.toml": b"[build]\nrustflags = []\n"}, ".cargo/config.toml"),
            ({"crates/review-pipeline/.cargo/config": b"[build]\n"},
             "crates/review-pipeline/.cargo/config"),
        )
        for index, (bridge, why) in enumerate(cases):
            with self.subTest(why=why):
                repo, files, documents = self.calibrated("changed-%d" % index)
                calibrated_manifest = records.manifest(files, documents)
                bridged = repo.commit("bridge", dict(bridge, **{
                    self.CLOSURE: b"// v%d\n" % (index + 3)}))
                git = gitrefs.Git(repo.path)
                base = tomlfile.loads(calibrated_manifest, "base")
                admission = {"kind": "equivalent_adapter", "equivalence_group": self.GROUP,
                             "adapter_files": [self.CLOSURE],
                             "calibrated_activation_path": self.RETAINED}
                with self.assertRaisesRegex(ContractError, why):
                    activation.protected_diff(git, documents[0]["authority_sha"], bridged,
                                              admission, dict(base, authority_sha=bridged),
                                              base)
        # A deleted lock is a missing expected input.
        repo, files, documents = self.calibrated("deleted-lock")
        repo.git("rm", "-q", "Cargo.lock")
        bridged = repo.commit("bridge", {self.CLOSURE: b"// v9\n"})
        base = tomlfile.loads(records.manifest(files, documents), "base")
        with self.assertRaisesRegex(ContractError, "has no Cargo.lock"):
            activation.protected_diff(
                gitrefs.Git(repo.path), documents[0]["authority_sha"], bridged,
                {"equivalence_group": self.GROUP, "adapter_files": [self.CLOSURE],
                 "calibrated_activation_path": self.RETAINED}, base, base)
        # The unchanged build closure and a named adapter file alone are admitted.
        repo, files, documents = self.calibrated("unchanged")
        bridged = repo.commit("bridge", {self.CLOSURE: b"// v10\n"})
        base = tomlfile.loads(records.manifest(files, documents), "base")
        self.assertEqual(activation.protected_diff(
            gitrefs.Git(repo.path), documents[0]["authority_sha"], bridged,
            {"equivalence_group": self.GROUP, "adapter_files": [self.CLOSURE],
             "calibrated_activation_path": self.RETAINED}, base, base), [self.CLOSURE])
        with self.assertRaisesRegex(ContractError, "not in the closure"):
            repo, files, documents = self.calibrated("extra")
            extra = "crates/review-pipeline/examples/perf-probe/extra.rs"
            bridged = repo.commit("bridge", {extra: b"//\n"})
            base = tomlfile.loads(records.manifest(files, documents), "base")
            activation.protected_diff(
                gitrefs.Git(repo.path), documents[0]["authority_sha"], bridged,
                {"equivalence_group": self.GROUP, "adapter_files": [extra],
                 "calibrated_activation_path": self.RETAINED}, base, base)


class Bridge(object):
    """The stand-in API's view of a bridge's two retained runs: their staged artifacts, each
    run's record, its attempts and each attempt's jobs, as the Performance workflow produced
    them unless a test says otherwise."""

    def __init__(self, root):
        self.evidence = root
        os.makedirs(root)
        self.artifacts = []
        self.workflow_runs = {}
        self.runs = {}
        self.attempts = {}
        self.jobs = {}
        self.next_id = 1

    def stage(self, run_dir, actions_run, attempt=1, **run):
        self.next_id = records.stage(run_dir, self.evidence, actions_run, self.artifacts,
                                     self.next_id)
        plan = contracts.read_file(os.path.join(run_dir, "run.json"), "run")
        head = run.pop("head_sha", plan["resolved"]["control_sha"])
        record = {"id": actions_run, "run_number": actions_run, "run_attempt": attempt,
                  "display_title": "Performance", "event": "workflow_dispatch",
                  "path": fixtures.WORKFLOW, "head_branch": "main", "head_sha": head,
                  "created_at": "2026-11-09T05:00:00Z",
                  "run_started_at": "2026-11-09T05:00:01Z",
                  "updated_at": "2026-11-09T05:59:00Z", "status": "completed",
                  "conclusion": "success", "repository": {"full_name": fixtures.REPOSITORY}}
        record.update(run)
        self.runs[actions_run] = record
        lab = plan["run_id"]
        label = plan["profile"]["environment"]["runner_label"]
        names = ["build (%s)" % lab] + ["shard (%s %d)" % (lab, shard["index"])
                                        for shard in plan["shards"]]
        self.jobs[(actions_run, attempt)] = [
            {"id": actions_run * 100 + index, "name": name, "run_id": actions_run,
             "run_attempt": attempt, "head_sha": head, "status": "completed",
             "conclusion": "success", "labels": [label],
             "runner_group_name": "GitHub Actions", "runner_name": "GitHub Actions %d" % (
                 index + 1)} for index, name in enumerate(names)]

    def attempt(self, actions_run, number):
        return dict(self.runs[actions_run], run_attempt=number,
                    **self.attempts.get((actions_run, number), {}))


if __name__ == "__main__":
    unittest.main()
