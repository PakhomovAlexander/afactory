"""The main report links validated diagnostic child runs (ADR-0177).

Child runs are produced by the actual `Profile` driver over the stand-in tools of
tests.diagnostic_fixtures, and the parent's evidence is synthetic: nothing here is a real
capture, profile or measurement. What is exercised is the report's admission of child bundles
by parent run, plan, sources, selection and artifact hashes, how each case's profile and trace
state is shown, and the refusal of unrelated, tampered or unsafe children, of children whose
cases are not the parent's workloads or whose kind is not their frozen recipe's, and that only
sanitized SVG is ever linked (ADR-0179).
"""
import hashlib
import json
import lzma
import os
import re
import shutil
import struct
from fractions import Fraction
from unittest import mock

from perflab import (bundle, contracts, diagnostics, hostobserver, perfcapture, phasetrace, report,
                     symbolarchive, synthetic, toolrun)
from perflab import suite as suite_module
from perflab.values import ContractError, document_bytes
from tests import diagnostic_fixtures as fakes
from tests import test_diagnostics as diag
from tests import test_report
from tests.support import read, writable_copy

SMALL = diag.SMALL


def tree_digest(root):
    digest = hashlib.sha256()
    for directory, _, files in sorted(os.walk(root)):
        for name in sorted(files):
            path = os.path.join(directory, name)
            digest.update(os.path.relpath(path, root).encode() + b"\0" + read(path))
    return digest.hexdigest()


class LinkedChildren(diag.ChildRunBase):

    def clean(self, names=("probe-bin", "supervisor", "af-bin")):
        for name in names:
            shutil.rmtree(self.path(name), True)

    def made(self, name, recipe, tools, probe_config, cases=(SMALL,), **options):
        self.clean()
        doc, output = self.profile(recipe, list(cases), tools, probe_config, run_id=name,
                                   **options)
        target = self.path(name)
        os.rename(output, target)
        return doc, target

    def trace_child(self, name="trace_child"):
        # Overhead measured against a stand-in verdict probe: 10.5 ms traced against 10 ms is
        # a 5% overhead, above the 2% target, so the trace is labelled perturbing.
        plain = fakes.probe(self.path("plain"), self.golden, wall_ms={"plain": "10"})
        parents = dict((side, (plain, diag.sha256_bytes(read(plain))))
                       for side in diag.diagcontracts.SIDES)
        return self.made(name, "linux_gnu_trace", {},
                         {"trace": diag.ChildRun.TRACE, "wall_ms": {"trace": "10.5"}},
                         parent_probes=parents)

    def profile_child(self, name="profile_child"):
        self.clean(("perf", "inferno"))
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"))
        tools = diag.diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        return self.made(name, "linux_gnu_cpu_profile", tools,
                         {"setup_seconds": 0.05, "region_seconds": 0.05})

    def parent_run(self):
        return synthetic.write_run(self.parent, self.path("run"), lambda slot, case: 10)

    def report(self, children, name="report"):
        output = self.path(name)
        return report.write(self.parent_run() if not os.path.isdir(self.path("run"))
                            else self.path("run"), output, diagnostics=children), output

    def test_profile_and_trace_children_are_shown_and_linked(self):
        _, trace = self.trace_child()
        _, profile = self.profile_child()
        run = self.parent_run()
        before = tree_digest(run)
        doc, output = self.report([trace, profile])
        self.assertEqual(tree_digest(run), before, "the parent's bundle is never changed")
        self.assertEqual([child["run_id"] for child in doc["diagnostics"]],
                         ["trace_child", "profile_child"])
        by_case = dict((item["instance_id"], item) for item in doc["cases"])
        small = by_case[SMALL]
        self.assertEqual(small["profile"]["state"], "available", small["profile"])
        self.assertEqual(sorted(item["kind"] for item in small["profile"]["items"]),
                         ["baseline", "candidate", "difference_forward", "difference_reverse"])
        self.assertEqual(small["trace"]["state"], "available", small["trace"])
        kinds = sorted(set(item["kind"] for item in small["trace"]["items"]))
        self.assertEqual(kinds, ["perfetto", "timeline", "trace"])
        for item in small["profile"]["items"] + small["trace"]["items"]:
            # Every link is a verified copy below the report, with the hash the child named.
            data = read(os.path.join(output, *item["path"].split("/")))
            self.assertEqual("sha256:" + hashlib.sha256(data).hexdigest(), item["sha256"])
        for instance, item in by_case.items():
            if instance != SMALL:
                self.assertEqual((item["profile"]["state"], item["trace"]["state"]),
                                 ("unavailable", "unavailable"))
        page = read(os.path.join(output, report.REPORT_HTML)).decode("utf-8")
        test_report.assert_safe_page(self, page, output)
        self.assertIn("diagnostics/trace_child/diagnostic.html", page)
        self.assertTrue(all("perturbing" in item["note"] for item in small["trace"]["items"]))
        summary = read(os.path.join(output, report.SUMMARY_MD)).decode("utf-8")
        self.assertIn("#### Diagnostics", summary)
        self.assertIn("complete", summary)
        self.assertIn("perturbing", summary)
        # The child's own page is the detailed view, kept as its verified copy.
        self.assertTrue(os.path.isfile(os.path.join(output, "diagnostics", "profile_child",
                                                    "diagnostic.html")))
        reread = report.read(os.path.join(output, report.REPORT_JSON))
        self.assertEqual(reread, doc)

    def test_without_a_child_every_case_is_unavailable_and_says_so(self):
        doc, output = self.report([])
        self.assertEqual(doc["diagnostics"], [])
        for item in doc["cases"]:
            self.assertEqual(item["profile"]["state"], "unavailable")
            self.assertIn("report --diagnostic", item["profile"]["reason"])
        summary = read(os.path.join(output, report.SUMMARY_MD)).decode("utf-8")
        self.assertIn("No diagnostic child run is linked", summary)

    def test_omitted_and_partial_children_are_shown_as_they_are(self):
        outside = self.path("outside")
        with open(outside, "w") as stream:
            stream.write("not the candidate's")
        _, omitted = self.made("omitted_child", "linux_gnu_trace", {},
                               {"trace_symlink": outside})
        trace = {"schema": "af.perf-trace/1", "pid": 9, "clock": "monotonic_ns",
                 "epoch_unix_ns": "1", "capacity": 2, "dropped": 5, "open": 0,
                 "spans": [{"id": 1, "parent": None, "phase": "probe_setup", "tid": 1,
                            "start_ns": 10, "end_ns": 50}]}
        _, partial = self.made("partial_child", "linux_gnu_trace", {}, {"trace": trace})
        doc, _ = self.report([omitted])
        small = [item for item in doc["cases"] if item["instance_id"] == SMALL][0]
        self.assertEqual(small["trace"]["state"], "omitted")
        self.assertEqual(set(item["reason"] for item in small["trace"]["omissions"]),
                         {"trace_unadmitted"})
        doc, _ = self.report([partial], name="report-partial")
        small = [item for item in doc["cases"] if item["instance_id"] == SMALL][0]
        self.assertEqual(small["trace"]["state"], "partial")
        self.assertTrue(any("region_missing" in (item["note"] or "")
                            for item in small["trace"]["items"]))

    def test_unrelated_tampered_and_unsafe_children_are_refused(self):
        _, child = self.trace_child()
        self.parent_run()
        # Tampered: one byte of a trace changed after the bundle was sealed.
        tampered = self.path("tampered")
        shutil.copytree(child, tampered)
        trace = [os.path.join(d, n) for d, _, names in os.walk(tampered) for n in names
                 if n == "trace.json"][0]
        os.chmod(trace, 0o644)
        data = read(trace)
        with open(trace, "wb") as stream:
            stream.write(data[:-1] + (b" " if data[-1:] != b" " else b"\n"))
        with self.assertRaises(ContractError) as caught:
            self.report([tampered], name="r1")
        self.assertIn("does not match its manifest hash", str(caught.exception))
        # Re-sealed with a new manifest: the profiles document still names the old hash.
        os.remove(os.path.join(tampered, bundle.MANIFEST))
        bundle.write_manifest(tampered, 1 << 30)
        with self.assertRaises(ContractError) as caught:
            self.report([tampered], name="r2")
        self.assertIn("is not the verified member its profiles name", str(caught.exception))
        # Unsafe: a symlink inside the child.
        unsafe = self.path("unsafe")
        shutil.copytree(child, unsafe, symlinks=True)
        os.symlink("/etc/hosts", os.path.join(unsafe, "traces", "escape"))
        with self.assertRaises(ContractError) as caught:
            self.report([unsafe], name="r3")
        self.assertIn("symlink", str(caught.exception))
        # Unrelated: a child of another parent run.
        unrelated = self.path("unrelated")
        shutil.copytree(child, unrelated)
        profiles = os.path.join(unrelated, "profiles.json")
        doc = json.loads(read(profiles))
        doc["parent_run_id"] = "another_run"
        os.chmod(profiles, 0o644)
        with open(profiles, "w") as stream:
            json.dump(doc, stream, sort_keys=True, separators=(",", ":"))
        os.remove(os.path.join(unrelated, bundle.MANIFEST))
        bundle.write_manifest(unrelated, 1 << 30)
        with self.assertRaises(ContractError) as caught:
            self.report([unrelated], name="r4")
        self.assertIn("not of this run", str(caught.exception))
        for name in ("r1", "r2", "r3", "r4"):
            self.assertEqual(os.listdir(self.path(name)), [],
                             "nothing is written before every child is admitted")


ACTIVE_PAGE = (b"<!DOCTYPE html><html><head><title>x</title></head><body>"
               b"<script>window.__AF_CHILD_ADMISSION_MARKER__=1</script></body></html>\n")
ACTIVE_SVGS = {
    "inline script": b'<svg xmlns="http://www.w3.org/2000/svg"><script>window.__AF__=1</script>'
                     b'<rect width="1" height="1"/></svg>\n',
    "event attribute": b'<svg xmlns="http://www.w3.org/2000/svg" onload="window.__AF__=1">'
                       b'<rect width="1" height="1"/></svg>\n',
    "external reference": b'<svg xmlns="http://www.w3.org/2000/svg" '
                          b'xmlns:xlink="http://www.w3.org/1999/xlink"><image '
                          b'xlink:href="https://example.invalid/x.png" width="1" height="1"/>'
                          b'</svg>\n',
}


class Admission(LinkedChildren):
    """Hashes prove a child's integrity, never its safety or consistency (ADR-0178). Each
    negative here is hash-consistent: the edited child is resealed with a recomputed manifest,
    its profiles name the edited members' hashes and its plan digest is recomputed, exactly as
    the root reproductions did. Active content is data only: nothing is opened in a browser and
    nothing is fetched."""

    def no_capture_child(self, name="nocapture_child"):
        # No perf is configured: the actual driver records a perf_unavailable omission.
        return self.made(name, "linux_gnu_cpu_profile", {}, {})

    def resealed(self, child, name, edit):
        """A writable copy of `child` edited by `edit(directory)`, then resealed."""
        target = writable_copy(child, self.path(name))
        edit(target)
        os.remove(os.path.join(target, bundle.MANIFEST))
        bundle.write_manifest(target, 1 << 30)
        return target

    def refused(self, child, needle, name):
        run = self.path("run") if os.path.isdir(self.path("run")) else self.parent_run()
        before = (tree_digest(run), tree_digest(child))
        with self.assertRaises(ContractError) as caught:
            self.report([child], name=name)
        self.assertIn(needle, str(caught.exception))
        self.assertEqual(os.listdir(self.path(name)), [], "nothing is written before every "
                         "child is admitted")
        self.assertEqual((tree_digest(run), tree_digest(child)), before)

    @staticmethod
    def rewrite(directory, relative, data):
        with open(os.path.join(directory, *relative.split("/")), "wb") as stream:
            stream.write(data)

    @staticmethod
    def edit_documents(directory, plan_edit=None, profiles_edit=None, writer=True):
        """Edit the frozen child plan and its profiles, keeping both valid documents: the
        plan's digest is recomputed and the profiles name it. Without `writer`, the plan's
        canonical bytes are written even when its writer would refuse them."""
        plan_path = os.path.join(directory, diag.diagnostics.PLAN_FILE)
        profiles_path = os.path.join(directory, diag.diagnostics.PROFILES_FILE)
        plan = json.loads(read(plan_path))
        doc = json.loads(read(profiles_path))
        if plan_edit is not None:
            plan_edit(plan)
            plan["plan_digest"] = contracts.plan_digest(plan)
        doc["plan_digest"] = plan["plan_digest"]
        if profiles_edit is not None:
            profiles_edit(doc)
        with open(plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run") if writer
                         else document_bytes(plan))
        with open(profiles_path, "wb") as stream:
            stream.write(contracts.write_bytes(doc, "profiles"))

    def test_admitted_children_are_copied_as_they_are_and_nothing_original_changes(self):
        _, nocapture = self.no_capture_child()
        _, profile = self.profile_child()
        _, trace = self.trace_child()
        run = self.parent_run()
        before = [tree_digest(path) for path in (run, nocapture, profile, trace)]
        doc, output = self.report([nocapture, profile, trace])
        self.assertEqual([tree_digest(path) for path in (run, nocapture, profile, trace)],
                         before, "neither the parent nor a child bundle is changed")
        small = [item for item in doc["cases"] if item["instance_id"] == SMALL][0]
        self.assertEqual(small["profile"]["state"], "partial", "the no-capture child's "
                         "omission is kept beside the captured child's flamegraphs")
        self.assertEqual(len(small["profile"]["items"]), 4)
        self.assertIn("perf_unavailable",
                      [item["reason"] for item in small["profile"]["omissions"]])
        for child in (nocapture, profile, trace):
            name = os.path.basename(child)
            copied = os.path.join(output, report.DIAGNOSTICS, name)
            self.assertEqual(tree_digest(copied), tree_digest(child))
            page = read(os.path.join(copied, report.PAGE)).decode("utf-8")
            test_report.assert_safe_page(self, page, copied, images=True)

    def test_a_hash_consistent_page_that_is_not_the_renderers_is_refused(self):
        _, child = self.no_capture_child()
        page = read(os.path.join(child, report.PAGE))
        csp = page[page.index(b"<meta http-equiv"):page.index(b">", page.index(
            b"<meta http-equiv")) + 1]
        variants = {
            "script page": ACTIVE_PAGE,
            "event attribute": page.replace(b"<h1>", b'<img src="x" onerror="window.__AF__=1">'
                                            b"<h1>", 1),
            "external reference": page.replace(b"<h1>", b'<a href="https://example.invalid/">'
                                               b"x</a><h1>", 1),
            "no CSP": page.replace(csp, b"", 1),
        }
        for index, (what, data) in enumerate(sorted(variants.items())):
            with self.subTest(what=what):
                self.assertNotEqual(data, page)
                edited = self.resealed(child, "page-%d" % index,
                                       lambda target: self.rewrite(target, report.PAGE, data))
                self.refused(edited, "is not the page the trusted renderer draws",
                             "report-page-%d" % index)

    def test_a_hash_consistent_active_svg_is_refused(self):
        doc, child = self.profile_child()
        entry = doc["renders"][0]

        def replace(target, members, sanitized=True):
            for relative, data in members.items():
                self.rewrite(target, relative, data)

            def edit(profiles):
                for render in profiles["renders"]:
                    for key in ("original", "sanitized"):
                        if render[key]["path"] in members:
                            data = members[render[key]["path"]]
                            render[key].update(bytes=len(data), sha256=diag.sha256_bytes(data))
            self.edit_documents(target, profiles_edit=edit)

        index = 0
        for what, data in sorted(ACTIVE_SVGS.items()):
            for members in ({entry["sanitized"]["path"]: data},
                            {entry["sanitized"]["path"]: data, entry["original"]["path"]: data}):
                index += 1
                with self.subTest(what=what, members=sorted(members)):
                    edited = self.resealed(child, "svg-%d" % index,
                                           lambda target: replace(target, members))
                    self.refused(edited, "is not what the trusted sanitizer makes of its "
                                 "original", "report-svg-%d" % index)
        # An original the sanitizer refuses outright is no view either.
        dtd = (b'<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY x "y">]>'
               b'<svg xmlns="http://www.w3.org/2000/svg">&x;</svg>\n')
        edited = self.resealed(child, "svg-dtd", lambda target: replace(
            target, {entry["original"]["path"]: dtd}))
        self.refused(edited, "is no SVG the sanitizer admits", "report-svg-dtd")

    def test_a_hash_consistent_trace_export_that_is_not_its_trace_is_refused(self):
        doc, child = self.trace_child()
        trace = [item for item in doc["traces"] if item["omission"] is None][0]
        path = [item["path"] for item in trace["artifacts"]
                if item["path"].endswith("/perfetto.json")][0]
        data = b'{"traceEvents":[{"name":"<script>window.__AF__=1</script>","ph":"X"}]}\n'

        def edit(target):
            self.rewrite(target, path, data)

            def profiles(profiles_doc):
                for entry in profiles_doc["traces"]:
                    for artifact in entry["artifacts"]:
                        if artifact["path"] == path:
                            artifact.update(bytes=len(data), sha256=diag.sha256_bytes(data))
            self.edit_documents(target, profiles_edit=profiles)

        self.refused(self.resealed(child, "perfetto", edit), "is not the export of its trace",
                     "report-perfetto")

    def test_children_whose_documents_disagree_on_their_sources_are_refused(self):
        _, child = self.no_capture_child()
        other = self.repo.git("rev-parse", "HEAD^")
        self.assertNotEqual(other, self.parent["resolved"]["candidate_sha"])

        def candidate(plan):
            # The root reproduction: only the child plan names another candidate.
            plan["resolved"]["candidate_sha"] = other
            plan["selectors"]["head"] = other

        def sources(doc):
            doc["sources"]["candidate_sha"] = other

        def selectors(plan):
            plan["selectors"]["head"] = other

        def control(plan):
            plan["resolved"]["control_sha"] = other

        def authority(plan):
            plan["authority"]["policy_digest"] = "sha256:" + "0" * 64

        def recipe(doc):
            doc["recipe"]["recipe_digest"] = "sha256:" + "0" * 64

        def capture(doc):
            doc["capture"]["frequency_hz"] += 1

        def selection(doc):
            # An instance of the parent that the frozen child plan never selected.
            doc["selection"]["cases"].append({"instance_id": diag.VALIDATE,
                                              "case_id": diag.VALIDATE.split("__")[0],
                                              "reason": "explicit"})
            doc["budgets"]["cases"]["used"] = len(doc["selection"]["cases"])

        cases = (("candidate", candidate, None, "frozen child plan's resolved is not this run's"),
                 ("both", candidate, sources, "measures other sources than this run"),
                 ("selectors", selectors, None, "frozen child plan's selectors"),
                 ("control", control, None, "frozen child plan's resolved"),
                 ("authority", authority, None, "frozen child plan's authority"),
                 ("profiles-sources", None, sources, "measures other sources than this run"),
                 ("recipe", None, recipe, "its profiles claim recipe"),
                 ("capture", None, capture, "other capture bounds"),
                 ("selection", None, selection, "frozen child plan"))
        for name, plan_edit, profiles_edit, needle in cases:
            with self.subTest(name=name):
                edited = self.resealed(child, "identity-" + name, lambda target: (
                    self.edit_documents(target, plan_edit, profiles_edit)))
                self.refused(edited, needle, "report-identity-" + name)

    def test_a_captured_child_whose_plan_names_another_candidate_is_refused(self):
        _, child = self.profile_child()
        other = self.repo.git("rev-parse", "HEAD^")

        def candidate(plan):
            plan["resolved"]["candidate_sha"] = other
            plan["selectors"]["head"] = other

        edited = self.resealed(child, "captured-candidate", lambda target: (
            self.edit_documents(target, candidate)))
        self.refused(edited, "frozen child plan's resolved is not this run's",
                     "report-captured-candidate")

    # ADR-0179 ----------------------------------------------------------------------------

    @staticmethod
    def repaged(directory):
        """Redraw the child's page from its edited profiles and traces with the trusted
        renderer, so only the check under test can refuse it."""
        doc = json.loads(read(os.path.join(directory, diagnostics.PROFILES_FILE)))
        traces = []
        for entry in doc["traces"]:
            named = [artifact["path"] for artifact in entry["artifacts"]
                     if artifact["path"].endswith("/trace.json")]
            traces.append(None if entry["omission"] is not None else phasetrace.load(
                read(os.path.join(directory, *named[0].split("/"))), named[0]))
        with open(os.path.join(directory, report.PAGE), "wb") as stream:
            stream.write(diagnostics.page(doc, traces))

    def edited(self, child, name, plan_edit=None, profiles_edit=None, writer=True):
        """A hash-consistent edit: documents rewritten, digest recomputed, page redrawn by
        the renderer and the bundle resealed."""
        def edit(target):
            self.edit_documents(target, plan_edit, profiles_edit, writer)
            self.repaged(target)
        return self.resealed(child, name, edit)

    @staticmethod
    def linked(page):
        return re.findall(r'<(?:a|img)\s[^>]*(?:href|src)="([^"]*)"', page)

    def test_an_active_original_stays_archived_while_every_linked_svg_is_script_free(self):
        # The stand-in inferno writes the onload handler and <script> element inferno's real
        # --no-javascript output keeps: a valid original that is evidence, never a view.
        doc, child = self.profile_child()
        self.assertEqual(sorted(entry["kind"] for entry in doc["renders"]),
                         ["baseline", "candidate", "difference_forward", "difference_reverse"])
        run = self.parent_run()
        before = (tree_digest(run), tree_digest(child))
        report_doc, output = self.report([child])
        self.assertEqual((tree_digest(run), tree_digest(child)), before)
        copied = os.path.join(output, report.DIAGNOSTICS, os.path.basename(child))
        self.assertEqual(tree_digest(copied), tree_digest(child), "archived exactly")
        page = read(os.path.join(copied, report.PAGE)).decode("utf-8")
        main = read(os.path.join(output, report.REPORT_HTML)).decode("utf-8")
        for entry in doc["renders"]:
            original = read(os.path.join(copied, *entry["original"]["path"].split("/")))
            # The original's exact bytes are archived, active markup and all.
            self.assertEqual(diag.sha256_bytes(original), entry["original"]["sha256"])
            self.assertIn(b"<script", original)
            self.assertIn(b"onload=", original)
            # Named by path and hash as archived evidence, never linked from either page.
            self.assertIn(entry["original"]["path"], page)
            self.assertIn(entry["original"]["sha256"], page)
            for html in (page, main):
                self.assertNotIn(entry["original"]["path"], self.linked(html))
                self.assertNotIn(child_path(entry["original"]["path"], child), self.linked(html))
        # Every SVG either page links or embeds is a sanitized view, script-free.
        svgs = [path for path in self.linked(page) if path.endswith(".svg")]
        self.assertEqual(sorted(set(svgs)),
                         sorted(entry["sanitized"]["path"] for entry in doc["renders"]))
        test_report.assert_safe_page(self, page, copied, images=True)
        test_report.assert_safe_page(self, main, output)
        main_svgs = [path for path in self.linked(main) if path.endswith(".svg")]
        self.assertEqual(sorted(main_svgs), sorted(
            child_path(entry["sanitized"]["path"], child) for entry in doc["renders"]))
        # Baseline, candidate and both difference directions all remain available.
        small = [item for item in report_doc["cases"] if item["instance_id"] == SMALL][0]
        self.assertEqual(small["profile"]["state"], "available")
        self.assertEqual(sorted(item["kind"] for item in small["profile"]["items"]),
                         ["baseline", "candidate", "difference_forward", "difference_reverse"])
        # The child's own page, in its own bundle, links no original either.
        own = read(os.path.join(child, report.PAGE)).decode("utf-8")
        self.assertEqual(own, page)

    def test_a_child_case_whose_workload_is_not_its_parent_case_is_refused(self):
        seeded = [case for case in self.parent["cases"] if case["seed"] is not None][0]
        _, child = self.made("workload_child", "linux_gnu_cpu_profile", {}, {},
                             cases=(SMALL, seeded["instance_id"]))
        plan = json.loads(read(os.path.join(child, diagnostics.PLAN_FILE)))
        # The usable child plans its own orders: an allowed difference, which is admitted.
        parent_cases = dict((case["instance_id"], case) for case in self.parent["cases"])
        self.assertTrue(any(case[name] != parent_cases[case["instance_id"]][name]
                            for case in plan["cases"]
                            for name in report.DIAGNOSTIC_CASE_FIELDS))
        doc, _ = self.report([child], name="report-workload")
        self.assertEqual(doc["diagnostics"][0]["cases"], [SMALL, seeded["instance_id"]])

        def case(instance_id, edit):
            def plan_edit(plan):
                edit([item for item in plan["cases"] if item["instance_id"] == instance_id][0])
            return plan_edit

        def parameters(item):
            # The root reproduction: same instance and case IDs, another package.
            item["parameters"] = dict(item["parameters"], package="issue_255_medium")

        def seed(item):
            # Another writer contract over the same assignment.
            item["seed"] = dict(item["seed"], contract_id=item["seed"]["contract_id"] + "_other")

        def seeded_parameters(item):
            # A smaller seeded log under the same IDs, the seed renamed to match it.
            item["parameters"] = dict(item["parameters"], log_events=1000)
            item["seed"] = dict(item["seed"], parameters=dict(item["seed"]["parameters"],
                                                              log_events=1000))
            item["seed"]["seed_id"] = suite_module.instance_id(
                item["seed"]["case_id"], sorted(item["seed"]["parameters"].items()))

        def timing(item):
            item["timing"] = dict(item["timing"], region=item["timing"]["region"] + "_other")

        def oracle(item):
            item["oracle"] = dict(item["oracle"], kind="golden_cas_digest"
                                  if item["oracle"]["kind"] != "golden_cas_digest"
                                  else "golden_canonical_digest")

        def environment(item):
            item["af_environment"] = dict(item["af_environment"], AF_PERF_OTHER="1")

        def cache(item):
            item["cache"] = dict(item["cache"], projection="prewarmed"
                                 if item["cache"]["projection"] != "prewarmed"
                                 else "application_cold")

        cases = (("parameters", SMALL, parameters, "parameters"),
                 ("seed", seeded["instance_id"], seed, "seed"),
                 ("seeded-parameters", seeded["instance_id"], seeded_parameters,
                  "parameters, seed"),
                 ("timing", SMALL, timing, "timing"),
                 ("oracle", SMALL, oracle, "oracle"),
                 ("af-environment", SMALL, environment, "af_environment"),
                 ("cache", seeded["instance_id"], cache, "cache"))
        for name, instance_id, edit, field in cases:
            with self.subTest(name=name):
                edited = self.edited(child, "workload-" + name, case(instance_id, edit))
                contracts.validate_run(json.loads(read(os.path.join(
                    edited, diagnostics.PLAN_FILE))))
                self.refused(edited, "is not this run's workload: %s differs" % field,
                             "report-workload-" + name)

    def test_a_child_whose_kind_is_not_its_frozen_recipe_s_is_refused(self):
        outside = self.path("outside")
        with open(outside, "w") as stream:
            stream.write("not the candidate's")
        # The root reproduction: a no-capture trace child relabelled a CPU profile.
        _, trace = self.made("nocapture_trace", "linux_gnu_trace", {},
                             {"trace_symlink": outside})
        # The reviewer's: a no-capture CPU-profile child relabelled a trace.
        _, profile = self.no_capture_child()

        def kind(value):
            def edit(doc):
                doc["recipe"]["kind"] = value
            return edit

        def frozen(value):
            def edit(plan):
                plan["diagnostic"]["recipe"]["kind"] = value
            return edit

        def unfrozen(plan):
            plan["diagnostic"]["recipe"] = None

        cases = (("trace-to-cpu", trace, None, kind("cpu_profile"),
                  "its frozen child plan froze recipe linux_gnu_trace as a trace recipe"),
                 ("cpu-to-trace", profile, None, kind("trace"),
                  "froze recipe linux_gnu_cpu_profile as a cpu_profile recipe"),
                 ("cpu-to-counters", profile, None, kind("counters"),
                  "froze recipe linux_gnu_cpu_profile as a cpu_profile recipe"),
                 ("both-relabelled", profile, frozen("trace"), kind("trace"),
                  "a trace recipe has perf-trace"),
                 ("unfrozen", profile, unfrozen, None, "froze no diagnostic recipe"))
        for name, child, plan_edit, profiles_edit, needle in cases:
            with self.subTest(name=name):
                # A plan relabelled against its own recipe is one its writer never writes; it
                # is written raw here to show the reader refuses it too.
                writer = name != "both-relabelled"
                if not writer:
                    with self.assertRaises(ContractError):
                        self.edited(child, "kind-writer", plan_edit, profiles_edit)
                edited = self.edited(child, "kind-" + name, plan_edit, profiles_edit, writer)
                self.refused(edited, needle, "report-kind-" + name)

    def test_cpu_trace_and_counter_children_keep_their_classifications(self):
        _, profile = self.profile_child()
        _, trace = self.trace_child()
        tools = diag.diagnostics.configured_tools(valgrind_bin=fakes.valgrind(
            self.path("valgrind")))
        _, counters = self.made("counter_child", "linux_gnu_counters", tools,
                                {"setup_seconds": 0.05, "region_seconds": 0.05})
        doc, _ = self.report([profile, trace, counters])
        self.assertEqual([(child["run_id"], child["recipe_id"], child["kind"])
                          for child in doc["diagnostics"]],
                         [("profile_child", "linux_gnu_cpu_profile", "cpu_profile"),
                          ("trace_child", "linux_gnu_trace", "trace"),
                          ("counter_child", "linux_gnu_counters", "counters")])
        small = [item for item in doc["cases"] if item["instance_id"] == SMALL][0]
        self.assertEqual((small["profile"]["state"], small["trace"]["state"]),
                         ("available", "available"))
        self.assertEqual(set(item["child_run_id"] for item in small["profile"]["items"]),
                         {"profile_child"})
        self.assertEqual(set(item["child_run_id"] for item in small["trace"]["items"]),
                         {"trace_child"})
        # A counter child relabelled a CPU profile in both documents: its recipe keeps the
        # symbols, frame pointers and region handshake a CPU profile recipe needs, so only its
        # evidence, counters, shows it is no CPU profile; it is refused, never classified.
        def plan_kind(plan):
            plan["diagnostic"]["recipe"]["kind"] = "cpu_profile"

        def profiles_kind(profiles_doc):
            profiles_doc["recipe"]["kind"] = "cpu_profile"

        edited = self.edited(counters, "counters-as-cpu", plan_kind, profiles_kind)
        contracts.validate_run(json.loads(read(os.path.join(edited, diagnostics.PLAN_FILE))))
        self.refused(edited, "a cpu_profile child holds counters, which is not evidence of its "
                     "kind", "report-counters-as-cpu")


def child_path(relative, child):
    return "%s/%s/%s" % (report.DIAGNOSTICS, os.path.basename(child), relative)


class RetainedSymbols(diag.ChildRunBase):
    """Actual Profile writer through report admission, with sealed hostile archive edits."""

    clean = LinkedChildren.clean
    made = LinkedChildren.made
    profile_child = LinkedChildren.profile_child
    resealed = Admission.resealed

    def archive_child(self, distinct=False):
        perf = fakes.perf(self.path("perf"), phase_file=self.path("probe-bin", "phase"),
                          unknown_with_frame_pointers=2)
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        return self.profile("linux_gnu_cpu_profile", [SMALL], tools, {},
                            distinct_candidate=distinct)

    @staticmethod
    def rewrite(directory, doc):
        with open(os.path.join(directory, diagnostics.PROFILES_FILE), "wb") as stream:
            stream.write(contracts.write_bytes(doc, "profiles"))
        with open(os.path.join(directory, report.PAGE), "wb") as stream:
            stream.write(diagnostics.page(doc, []))

    def test_shared_archive_admits_four_captures_once_without_double_charging(self):
        doc, child = self.archive_child()
        self.assertEqual(len(doc["captures"]), 4)
        self.assertEqual(len(os.listdir(os.path.join(child, "symbols"))), 1)
        archives = [item["symbols"]["archive"] for item in doc["captures"]]
        self.assertTrue(all(item == archives[0] for item in archives))
        with mock.patch.object(symbolarchive.lzma, "LZMADecompressor",
                               wraps=lzma.LZMADecompressor) as decode:
            report.admit_child(child, self.parent)
        self.assertEqual(decode.call_count, 1)
        # The writer's recorded usage counts each physical evidence member once.
        sizes = sum(os.path.getsize(os.path.join(root, name))
                    for root, _, files in os.walk(child) for name in files
                    if name not in ("profiles.json", "diagnostic.html", "manifest.json"))
        self.assertEqual(doc["budgets"]["bytes"]["evidence_used"], sizes)
        # The report also copies the admitted child without changing the parent verdict.
        run = synthetic.write_run(self.parent, self.path("run"), lambda slot, case: 10)
        result = report.write(run, self.path("report"), diagnostics=[child])
        self.assertEqual(result["diagnostics"][0]["run_id"], doc["run_id"])

    def test_distinct_archives_admit_both_originals(self):
        doc, child = self.archive_child(distinct=True)
        self.assertEqual(len(os.listdir(os.path.join(child, "symbols"))), 2)
        self.assertEqual(len({item["binary_sha256"] for item in doc["captures"]}), 2)
        report.admit_child(child, self.parent)

    def test_resealed_missing_unrelated_corrupt_and_false_identity_archives_refuse(self):
        doc, child = self.archive_child()
        artifact = doc["captures"][0]["symbols"]["archive"]
        path = artifact["path"]
        original = read(os.path.join(child, path))
        variants = [("missing", None, "unrelated or missing"),
                    ("unrelated", lzma.compress(b"other executable"), "decompressed identity"),
                    ("truncated", original[:-1], "truncated XZ"),
                    ("concatenated", original + original, "trailing or concatenated"),
                    ("trailing", original + b"garbage", "trailing or concatenated"),
                    ("padding", original + bytes(4), "trailing or concatenated"),
                    ("corrupt", original[:30] + bytes([original[30] ^ 1]) + original[31:],
                     "invalid XZ")]
        for name, data, needle in variants:
            with self.subTest(name=name):
                def edit(target):
                    copied = json.loads(read(os.path.join(target, diagnostics.PROFILES_FILE)))
                    if data is None:
                        os.remove(os.path.join(target, path))
                    else:
                        with open(os.path.join(target, path), "wb") as stream:
                            stream.write(data)
                        for capture in copied["captures"]:
                            capture["symbols"]["archive"].update(bytes=len(data),
                                                                 sha256=diag.sha256_bytes(data))
                    self.rewrite(target, copied)
                target = self.resealed(child, "symbols-" + name, edit)
                with self.assertRaisesRegex(ContractError, needle):
                    report.admit_child(target, self.parent)
        # A valid but unclaimed archive is refused even when the manifest verifies it.
        def extra(target):
            with open(os.path.join(target, "symbols", "unrelated.xz"), "wb") as stream:
                stream.write(original)
        target = self.resealed(child, "extra-symbols", extra)
        with self.assertRaisesRegex(ContractError, "unrelated or missing"):
            report.admit_child(target, self.parent)

    def test_archive_integrity_size_and_resource_limits_are_independent(self):
        doc, child = self.archive_child()
        artifact = doc["captures"][0]["symbols"]["archive"]
        path = artifact["path"]
        def tamper(target):
            with open(os.path.join(target, path), "ab") as stream:
                stream.write(b"x")
        target = self.resealed(child, "tamper-symbols", tamper)
        with self.assertRaisesRegex(ContractError, "not the verified member"):
            report.admit_child(target, self.parent)
        # Without resealing, the ordinary manifest integrity check refuses first.
        with open(os.path.join(child, path), "ab") as stream:
            stream.write(b"x")
        with self.assertRaisesRegex(ContractError, "manifest says"):
            report.admit_child(child, self.parent)
        with open(os.path.join(child, path), "r+b") as stream:
            stream.truncate(artifact["bytes"])
        for constant, value, needle in (("DECOMPRESSED_BYTES", 100, "decompressed byte bound"),
                                         ("MEMORY_BYTES", 100, "invalid XZ"),
                                         ("SECONDS", 0, "deadline")):
            with self.subTest(bound=constant), mock.patch.object(symbolarchive, constant, value):
                with self.assertRaisesRegex(ContractError, needle):
                    report.admit_child(child, self.parent)
        def size(target):
            copied = json.loads(read(os.path.join(target, diagnostics.PROFILES_FILE)))
            for capture in copied["captures"]:
                capture["symbols"]["archive"]["bytes"] += 1
            self.rewrite(target, copied)
        target = self.resealed(child, "size-symbols", size)
        with self.assertRaisesRegex(ContractError, "archive size differs"):
            report.admit_child(target, self.parent)

    def test_referenced_missing_and_retained_claims_are_strict(self):
        doc, child = self.archive_child()
        for state in ("referenced", "missing"):
            copied = json.loads(json.dumps(doc))
            copied["captures"][0]["symbols"]["state"] = state
            with self.assertRaisesRegex(ContractError, "exactly retained"):
                contracts.write_bytes(copied, "profiles")
        copied = json.loads(json.dumps(doc))
        copied["captures"][0]["symbols"]["archive"] = None
        with self.assertRaisesRegex(ContractError, "exactly retained"):
            contracts.write_bytes(copied, "profiles")
        copied = json.loads(json.dumps(doc))
        copied["captures"][0]["symbols"]["archive"]["path"] = "symbols/unrelated.xz"
        with self.assertRaisesRegex(ContractError, "content-addressed"):
            contracts.write_bytes(copied, "profiles")
        copied = json.loads(json.dumps(doc))
        copied["captures"][0]["symbols"]["archive"]["bytes"] += 1
        with self.assertRaisesRegex(ContractError, "shared evidence agrees"):
            contracts.write_bytes(copied, "profiles")
        # Legitimate referenced-only evidence remains admissible with no retained archive.
        for capture in doc["captures"]:
            capture["symbols"].update(state="referenced", archive=None)
        def referenced(target):
            shutil.rmtree(os.path.join(target, "symbols"))
            self.rewrite(target, doc)
        target = self.resealed(child, "references-only", referenced)
        report.admit_child(target, self.parent)


    def test_collection_rejects_false_identity_and_expired_admission(self):
        doc, child = self.archive_child()
        for name in ("false-identity", "expired"):
            with self.subTest(case=name):
                target = writable_copy(child, self.path(name))
                for filename in (diagnostics.PROFILES_FILE, report.PAGE, bundle.MANIFEST):
                    os.remove(os.path.join(target, filename))
                copied = json.loads(json.dumps(doc))
                if name == "false-identity":
                    archive = lzma.compress(b"unrelated executable")
                    path = copied["captures"][0]["symbols"]["archive"]["path"]
                    with open(os.path.join(target, path), "wb") as stream:
                        stream.write(archive)
                    for capture in copied["captures"]:
                        capture["symbols"]["archive"].update(bytes=len(archive),
                                                           sha256=diag.sha256_bytes(archive))
                    with self.assertRaisesRegex(ContractError, "decompressed identity"):
                        diagnostics.finish(target, copied, [])
                else:
                    budget = diagnostics.Budget(self.registry["limits"], target)
                    budget.deadline = budget.clock() - 1
                    with self.assertRaisesRegex(ContractError, "deadline"):
                        diagnostics.finish(target, copied, [], budget)

    def test_compressed_limit_and_rehash_apply_after_manifest_verification(self):
        doc, child = self.archive_child()
        _, hashes = bundle.verify(child, diag.diagnostics.limits.DIAGNOSTIC_BYTES_MAX)
        artifact = doc["captures"][0]["symbols"]["archive"]
        copied = json.loads(json.dumps(doc))
        copied["budgets"]["bytes"]["limit"] = artifact["bytes"] - 1
        with self.assertRaisesRegex(ContractError, "compressed byte bound"):
            symbolarchive.admit(child, copied, hashes)
        # Valid XZ of the same original still has to match the admitted compressed hash.
        path = os.path.join(child, artifact["path"])
        data = lzma.compress(lzma.decompress(read(path)), check=lzma.CHECK_SHA256)
        with open(path, "wb") as stream:
            stream.write(data)
        for capture in doc["captures"]:
            capture["symbols"]["archive"]["bytes"] = len(data)
        with self.assertRaisesRegex(ContractError, "compressed archive hash differs"):
            symbolarchive.admit(child, doc, hashes)


# A host-observed recording as the trusted perf writes it, for `RepeatedCaptures`: its header,
# one event attribute whose task records carry TID, TIME and CPU (`sample_id_all`), the stand-in
# perf's samples, a synthesized COMM of the waiting gate (no exec flag), the kernel's COMM of
# the gate executing the command and its EXIT, and another process's EXIT.
SAMPLE_TYPE = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 7) | (1 << 5)


def trailer(pid, when):
    return struct.pack("<IIQII", pid, pid, when, 0, 0)


def observed_recording(pid, executed, exited, samples, event, exec_flag=True, compressed=False,
                       sample_id_all=True):
    attr = struct.pack("<IIQQQQQ", 1, 48, 0, 997, SAMPLE_TYPE, 0,
                       (1 << 18) if sample_id_all else 0) + struct.pack("<QQ", 0, 0)
    records = b""
    body = json.dumps({"samples": samples, "unwind": "fp", "event": event}).encode()
    records += struct.pack("<IHH", 99, 0, 8 + len(body)) + body
    gate = struct.pack("<II", pid, pid) + b"sh\0\0\0\0\0\0" + trailer(pid, 1)
    records += struct.pack("<IHH", 3, 0, 8 + len(gate)) + gate
    command = struct.pack("<II", pid, pid) + b"af\0\0\0\0\0\0" + trailer(pid, executed)
    records += struct.pack("<IHH", 3 if not compressed else 81, (1 << 13) if exec_flag else 0,
                           8 + len(command)) + command
    other = struct.pack("<IIIIQ", pid + 1, pid, pid + 1, pid, 5) + trailer(pid + 1, 5)
    records += struct.pack("<IHH", 4, 0, 8 + len(other)) + other
    ended = struct.pack("<IIIIQ", pid, 1, pid, 1, exited) + trailer(pid, exited)
    records += struct.pack("<IHH", 4, 0, 8 + len(ended)) + ended
    header = b"PERFILE2" + struct.pack("<QQ", 104, len(attr)) + struct.pack("<QQ", 104, len(attr))
    header += struct.pack("<QQ", 104 + len(attr), len(records)) + struct.pack("<QQ", 0, 0)
    header += bytes(32)
    return header + attr + records


def lifetime(data, pid):
    return perfcapture.decode_lifetime(lambda offset, count: data[offset:offset + count],
                                       len(data), pid)


class ObservedBoundary(diag.unittest.TestCase):
    """The host-observed command's own time: its exec to its exit in the trusted recording."""

    def test_the_command_is_bounded_by_its_exec_and_exit_never_the_gate(self):
        data = observed_recording(4242, 1000000, 4500000, ["af"], "task-clock:u")
        self.assertEqual(lifetime(data, 4242), (1000000, 4500000))

    def test_a_recording_that_does_not_bound_the_command_refuses(self):
        cases = [("another process", observed_recording(4242, 10, 20, [], "e"), 7, "no exec"),
                 ("no exec flag", observed_recording(4242, 10, 20, [], "e", exec_flag=False),
                  4242, "no exec"),
                 ("compressed", observed_recording(4242, 10, 20, [], "e", compressed=True),
                  4242, "compressed records"),
                 ("no record times", observed_recording(4242, 10, 20, [], "e",
                                                        sample_id_all=False), 4242,
                  "carry no time"),
                 ("exit before exec", observed_recording(4242, 30, 20, [], "e"), 4242,
                  "exited before"),
                 ("cut short", observed_recording(4242, 10, 20, [], "e")[:-9], 4242,
                  "unfinished or malformed header"),
                 ("not perf.data", b"x" * 200, 4242, "not a little-endian")]
        overrun = bytearray(observed_recording(4242, 10, 20, [], "e"))
        # The last record, the EXIT of 56 bytes, claims 8 bytes beyond its data section.
        struct.pack_into("<H", overrun, len(overrun) - 56 + 6, 64)
        cases.append(("record past its section", bytes(overrun), 4242, "data section"))
        for name, data, pid, needle in cases:
            with self.subTest(name=name):
                with self.assertRaises(perfcapture.CaptureError) as caught:
                    lifetime(data, pid)
                self.assertEqual(caught.exception.reason, "command_boundary_unknown")
                self.assertIn(needle, caught.exception.detail)
                self.assertIn("none of it is attributed", caught.exception.detail)


class RepeatedCaptures(diag.ChildRunBase):
    """A CLI CPU profile of many fresh, host-observed processes through the actual Profile
    writer, profiles.json, its bundle and report admission: 33 repetitions per side keep 66
    raw recordings and observer records each, beyond any 64-item list, all listed by the
    capture's verified index. The contained command and the host observer are injected:
    each repetition's recording is the trusted perf's, written where the observer records it,
    and its supervised child spends 50 ms more at the gate than its command runs."""

    clean = LinkedChildren.clean
    made = LinkedChildren.made
    resealed = Admission.resealed
    REPETITIONS = 33
    GATE_NS = 50000000

    def observed_child(self, **perf_config):
        registry = json.loads(json.dumps(self.registry))
        registry["capture"]["minimum_samples"] = 20
        registry["capture"]["max_repetitions"] = self.REPETITIONS
        perf = fakes.perf(self.path("perf"), **perf_config)
        tools = diagnostics.configured_tools(perf=perf, inferno_bin=fakes.inferno(
            self.path("inferno")))
        test, made, runs = self, [], []

        def observer(profile, staged, unwind, room, gate_root, whole, command_seconds):
            profile.observers += 1
            tag = "%04d-%s" % (profile.observers, os.urandom(4).hex())
            control = test.path("observer-control", tag)
            os.makedirs(control)
            item = hostobserver.Observer(None, perf, profile.event, profile.frequency, unwind,
                                         staged, control, os.path.join(gate_root, "gate-" + tag),
                                         profile.budget.deadline, whole)
            item.make_gate()
            # What `attach` froze and confirmed, and its perf's confirmed end.
            item.target = hostobserver.Target(5000 + profile.observers, 77, os.getuid(),
                                               "sha256:" + "c" * 64, "f" * 64, 1)
            item.attached_ms, item.released = 3, True
            item.result = toolrun.Result([perf], code=0)
            made.append(item)
            return item

        def cli_run(profile, side, case, binary, wrap=None, writes=(), file_bytes=None,
                    observe=None, what=None, **_):
            item = made[-1]
            self.assertTrue(hostobserver.is_gated(wrap(["af", "--help"])))
            spent = 3000000 + 1000 * len(runs)
            runs.append(spent)
            executed = 10 ** 9 * len(runs)
            with open(item.output, "xb") as stream:
                stream.write(observed_recording(item.target.pid, executed, executed + spent,
                                                ["af"] * 2, profile.event))
            reply = {"timing": {"child_wall_ns": spent + self.GATE_NS}, "setup": None}
            result = toolrun.Result(["af"], code=0)
            result.supervised = type("Supervised", (), {"result": reply})()
            return result, "unused", None

        with mock.patch.object(diagnostics.Profile, "host", "host"), \
                mock.patch.object(diagnostics.Profile, "trusted_argv", lambda self, argv: False), \
                mock.patch.object(diagnostics.Profile, "observer", observer), \
                mock.patch.object(diagnostics.Profile, "cli_run", cli_run):
            doc, child = self.profile("linux_gnu_cpu_profile", [diag.DISCOVERY], tools, {},
                                      registry=registry, af_config={}, duration_ms=10000)
        return doc, child, runs

    def index_of(self, child, capture):
        return contracts.read_file(os.path.join(child, capture["repetitions"]["path"]),
                                   "repetitions")

    def decodings(self):
        """The argv after `perf` of every `perf script` the fake perf ran, in order."""
        return [json.loads(line) for line in
                read(self.path("perf", "script-argv.jsonl")).decode().splitlines()]

    def test_many_observed_repetitions_are_indexed_serialized_and_admitted_offline(self):
        doc, child, runs = self.observed_child()
        self.assertEqual(len(doc["captures"]), 2, "both sides")
        self.assertEqual(len(runs), 2 * self.REPETITIONS)
        for side, capture in zip(diagnostics.diagcontracts.SIDES, doc["captures"]):
            self.assertEqual(capture["side"], side)
            self.assertIsNone(capture["omission"], capture["omission"])
            self.assertEqual((capture["region"]["repetitions"], capture["region"]["stopped"]),
                             (self.REPETITIONS, "repetition_cap"))
            directory = os.path.join(child, diagnostics.diagcontracts.capture_directory(
                capture))
            raw = [name for name in os.listdir(directory)
                   if diagnostics.diagcontracts.repeated_name(name)]
            self.assertEqual(len(raw), 2 * self.REPETITIONS, "every recording and observer "
                             "record is kept")
            self.assertEqual(sorted(item["path"].rsplit("/", 1)[1]
                                    for item in capture["artifacts"]),
                             ["capture.script", "collapsed.folded"])
            index = self.index_of(child, capture)
            self.assertEqual(index["boundary"], "perf_exec_to_exit")
            self.assertEqual(len(index["repetitions"]), self.REPETITIONS)
            self.assertTrue(all(item["completed"] and item["observer"] is not None
                                for item in index["repetitions"]))
            # The commands' own time: the 50 ms each one waited at the gate is not counted.
            spent = runs[:self.REPETITIONS] if side == "baseline" else runs[self.REPETITIONS:]
            self.assertEqual([item["region_ns"] for item in index["repetitions"]], spent)
            self.assertEqual(Fraction(capture["region"]["region_ms"]),
                             Fraction(sum(spent), 1000000))
            self.assertEqual(capture["samples"], 2 * self.REPETITIONS)
            script = read(os.path.join(directory, "capture.script")).decode()
            self.assertEqual(script.count("task-clock"), 2 * self.REPETITIONS)
            # Every listed recording, in its index's order, by its own function-level decoding.
            listed = [os.path.join(child, item["recording"]["path"])
                      for item in index["repetitions"]]
            self.assertEqual([argv for argv in self.decodings()
                              if os.path.dirname(argv[argv.index("-i") + 1]) == directory],
                             [perfcapture.script_argv("perf", path)[1:] for path in listed])
        self.assertEqual(sorted(item["kind"] for item in doc["renders"]),
                         ["baseline", "candidate", "difference_forward", "difference_reverse"])
        report.admit_child(child, self.parent)
        # A copy elsewhere is self-contained and verifies offline, and so does the report's
        # own copy of it.
        copied = shutil.copytree(child, self.path("copied"))
        admitted = report.admit_child(copied, self.parent)
        os.makedirs(self.path("offline"))
        report.copy_child(admitted, self.path("offline"))
        nested = os.path.join(self.path("offline"), *admitted.path("").split("/"))
        report.admit_child(nested, self.parent)

    def test_a_missing_changed_or_unlisted_repetition_refuses(self):
        doc, child, _ = self.observed_child()
        capture = doc["captures"][1]
        index = self.index_of(child, capture)
        recording = index["repetitions"][40 % self.REPETITIONS]["recording"]["path"]
        observer = index["repetitions"][2]["observer"]["path"]
        directory = diagnostics.diagcontracts.capture_directory(capture)

        def remove(path):
            return lambda target: os.remove(os.path.join(target, path))

        def flip(path):
            def edit(target):
                data = bytearray(read(os.path.join(target, path)))
                data[-1] ^= 1
                with open(os.path.join(target, path), "wb") as stream:
                    stream.write(bytes(data))
            return edit

        def extra(target):
            shutil.copy(os.path.join(target, recording),
                        os.path.join(target, directory, "rep-0099.data"))

        def misplaced(target):
            # A recording of this capture filed under the other side's directory.
            shutil.copy(os.path.join(target, recording),
                        os.path.join(target, directory.replace("candidate", "baseline"),
                                     "rep-0034.data"))
        for name, edit, needle in (
                ("missing recording", remove(recording), "not the verified member its index"),
                ("missing observer", remove(observer), "not the verified member its index"),
                ("changed recording", flip(recording), "not the verified member its index"),
                ("changed observer", flip(observer), "not the verified member its index"),
                ("changed index", flip(capture["repetitions"]["path"]),
                 "not the index its capture names"),
                ("unlisted repetition", extra, "beyond its index"),
                ("misplaced repetition", misplaced, "beyond its index")):
            with self.subTest(name=name):
                target = self.resealed(child, name.replace(" ", "-"), edit)
                with self.assertRaisesRegex(ContractError, needle):
                    report.admit_child(target, self.parent)

    def test_a_region_the_recording_does_not_bound_is_refused_offline(self):
        doc, child, _ = self.observed_child()
        capture = doc["captures"][0]
        index = self.index_of(child, capture)
        first = index["repetitions"][0]

        def edit(target):
            # A recording whose command took 1 ns more than its sealed index says.
            path = os.path.join(target, first["recording"]["path"])
            data = read(path)
            observer = contracts.read_file(os.path.join(target, first["observer"]["path"]),
                                           "host-observer")
            executed, exited = lifetime(data, observer["target"]["pid"])
            with open(path, "wb") as stream:
                # The EXIT record's own time, before its 24-byte sample_id trailer.
                stream.write(data[:-32] + struct.pack("<Q", exited + 1) + data[-24:])
            first["recording"].update(bytes=len(data), sha256=diag.sha256_bytes(read(path)))
            index_path = os.path.join(target, capture["repetitions"]["path"])
            with open(index_path, "wb") as stream:
                stream.write(contracts.write_bytes(index, "repetitions"))
            copied = json.loads(read(os.path.join(target, diagnostics.PROFILES_FILE)))
            copied["captures"][0]["repetitions"].update(
                bytes=os.path.getsize(index_path), sha256=diag.sha256_bytes(read(index_path)))
            RetainedSymbols.rewrite(target, copied)
        target = self.resealed(child, "unbounded", edit)
        with self.assertRaisesRegex(ContractError, "bounds its command at"):
            report.admit_child(target, self.parent)

    def test_a_failed_decoding_keeps_its_cause_and_every_recording_in_a_valid_report(self):
        doc, child, _ = self.observed_child(fail_script="rep-0005.data")
        for capture in doc["captures"]:
            self.assertEqual(capture["omission"]["reason"], "tool_failed")
            detail = capture["omission"]["detail"]
            self.assertIn("recording 5 of %d" % self.REPETITIONS, detail)
            self.assertIn("rep-0005.data", detail)
            self.assertIn("keeps the 4 recordings decoded before it", detail)
            directory = os.path.join(child, diagnostics.diagcontracts.capture_directory(
                capture))
            # Decoding stopped at the failed recording: no later one, and no retry of it.
            self.assertEqual([os.path.basename(argv[argv.index("-i") + 1])
                              for argv in self.decodings()
                              if os.path.dirname(argv[argv.index("-i") + 1]) == directory],
                             ["rep-%04d.data" % number for number in range(1, 6)])
            self.assertEqual(sorted(item["path"].rsplit("/", 1)[1]
                                    for item in capture["artifacts"]),
                             ["capture.script", "capture.script.part"])
            index = self.index_of(child, capture)
            self.assertEqual(len(index["repetitions"]), self.REPETITIONS)
        self.assertEqual(doc["renders"], [])
        report.admit_child(child, self.parent)

    def test_a_perf_refusing_function_level_decoding_leaves_every_recording_unrendered(self):
        self.REPETITIONS = 3
        doc, child, _ = self.observed_child(reject_no_inline=True)
        for capture in doc["captures"]:
            self.assertEqual(capture["omission"]["reason"], "tool_failed")
            self.assertIn("recording 1 of 3", capture["omission"]["detail"])
            self.assertIsNone(capture["samples"])
            self.assertEqual(len(self.index_of(child, capture)["repetitions"]), 3)
        self.assertTrue(all("--no-inline" in argv for argv in self.decodings()))
        self.assertEqual(doc["renders"], [])
        report.admit_child(child, self.parent)

    AF_ID = "cbc6452c61751405d91560eb9b6784a97da6faaa"
    LIBC_ID = "79005c16293efa45b441fed45f4f29b138557e9e"
    # Enough repetitions to reach the minimum samples; the many-recordings bound is above.
    FEW = 10

    def listed_child(self, last, **perf_config):
        """A capture of `FEW` repetitions whose `perf buildid-list --with-hits` of the first, a
        short execution, hit nothing, of the others only libc, of the last `last`; the
        profiled executable's own build ID is the AF one."""
        self.REPETITIONS = self.FEW
        listed = dict(("rep-%04d.data" % number, ["%s /usr/lib/libc.so.6" % self.LIBC_ID])
                      for number in range(2, self.FEW))
        listed["rep-0001.data"] = []
        listed["rep-%04d.data" % self.FEW] = last
        listed.update(perf_config.pop("build_ids", {}))
        with mock.patch.object(perfcapture, "elf_build_id", return_value=self.AF_ID):
            return self.observed_child(build_ids=listed, **perf_config)

    def test_a_later_recordings_hit_of_the_executable_is_found_when_the_first_has_none(self):
        doc, child, _ = self.listed_child(["%s /work/af" % self.AF_ID])
        for capture in doc["captures"]:
            self.assertIsNone(capture["omission"], capture["omission"])
            self.assertEqual(capture["symbols"]["build_ids"],
                             sorted([self.AF_ID, self.LIBC_ID]))
            self.assertIs(capture["symbols"]["binary_build_id_present"], True)
            self.assertEqual(len(self.index_of(child, capture)["repetitions"]), self.FEW)
        report.admit_child(child, self.parent)

    def test_no_recordings_hit_of_the_executable_is_never_inferred_from_the_executable(self):
        doc, child, _ = self.listed_child([])
        for capture in doc["captures"]:
            self.assertIsNone(capture["omission"], capture["omission"])
            self.assertEqual(capture["symbols"]["build_ids"], [self.LIBC_ID])
            self.assertIs(capture["symbols"]["binary_build_id_present"], False)
        report.admit_child(child, self.parent)

    def test_a_failed_or_malformed_build_id_listing_refuses_the_capture_by_its_recording(self):
        for name, config, reason in (
                ("failed", {"fail_buildids": "rep-0007.data"}, "tool_failed"),
                ("malformed", {"build_ids": {"rep-0007.data": ["not a build id line"]}},
                 "malformed_build_ids")):
            with self.subTest(name=name):
                for directory in ("child", "perf", "inferno", "observer-control"):
                    shutil.rmtree(self.path(directory), True)
                self.clean()
                doc, child, _ = self.listed_child(["%s /work/af" % self.AF_ID], **config)
                for capture in doc["captures"]:
                    self.assertEqual(capture["omission"]["reason"], reason)
                    detail = capture["omission"]["detail"]
                    self.assertIn("perf buildid-list of recording 7 of %d" % self.FEW, detail)
                    self.assertIn("rep-0007.data", detail)
                    # Every raw recording and observer record is still kept and indexed.
                    self.assertEqual(len(self.index_of(child, capture)["repetitions"]),
                                     self.FEW)
                    directory = os.path.join(child, diagnostics.diagcontracts.capture_directory(
                        capture))
                    self.assertEqual(len([item for item in os.listdir(directory)
                                          if diagnostics.diagcontracts.repeated_name(item)]),
                                     2 * self.FEW)
                self.assertEqual(doc["renders"], [])
                report.admit_child(child, self.parent)

    def test_a_repetition_its_recording_does_not_bound_stops_the_capture_honestly(self):
        with mock.patch.object(perfcapture, "decode_lifetime",
                               side_effect=perfcapture.CaptureError(
                                   "command_boundary_unknown", "it holds no exec of it")):
            doc, child, runs = self.observed_child()
        self.assertEqual(len(runs), 2, "one repetition per side, then the capture stops")
        for capture in doc["captures"]:
            self.assertEqual(capture["omission"]["reason"], "command_boundary_unknown")
            self.assertEqual((capture["region"]["repetitions"], capture["region"]["region_ms"]),
                             (0, "0"))
            index = self.index_of(child, capture)
            self.assertEqual([(item["completed"], item["region_ns"])
                              for item in index["repetitions"]], [(False, None)])
            self.assertIsNotNone(index["repetitions"][0]["recording"])
        report.admit_child(child, self.parent)
