"""Offline reports of one run (package p4, ADR-0172).

Current-format runs from the synthetic generator, labeled synthetic in every shard, so none of
these numbers describes the AF product; the measured-runner path is exercised by
tests.test_history over real shard bundles of the actual runner with stand-in binaries.
"""
import base64
import hashlib
import html
import json
import os
import re
import subprocess
import sys
from fractions import Fraction

from perflab import compare, contracts, render, report, schedule, svgsafe, synthetic
from perflab.values import ContractError
from tests.support import CLI, TempDir, read, run_dir, synthetic_suite

INJECTION = ('<script>alert("x")</script>"><img src=x onerror=alert(2)> | `pwned` '
             '[link](https://example.invalid) </style><!--')


TAGS = {"html", "head", "meta", "title", "style", "body", "h1", "h2", "h3", "section", "p",
        "table", "thead", "tbody", "tr", "th", "td", "span", "code", "a", "input", "script", "svg",
        "line", "circle", "text", "details", "summary", "ul", "li"}


def scripts_and_styles(page):
    return (re.findall(r"<script>(.*?)</script>", page, re.S),
            re.findall(r"<style>(.*?)</style>", page, re.S))


def csp_hash(text):
    return "'sha256-%s'" % base64.b64encode(hashlib.sha256(text.encode()).digest()).decode()


def assert_safe_page(test, page, output, images=False):
    """One hash-authorized script and style, no other active content, and every link a file
    the renderer placed below the page's own directory; with `images`, a diagnostic page's
    local images of its sanitized SVG and its phase timelines' rectangles too."""
    scripts, styles = scripts_and_styles(page)
    test.assertEqual(len(scripts), 1)
    test.assertEqual(len(styles), 1)
    test.assertEqual(page.count("<script"), 1)
    test.assertEqual(page.count("<style"), 1)
    csp = re.search(r'http-equiv="Content-Security-Policy" content="([^"]*)"', page)
    policy = csp.group(1).replace("&#x27;", "'")
    test.assertEqual(policy, render.CSP)
    test.assertIn("script-src %s;" % csp_hash(scripts[0]), policy)
    test.assertIn("style-src %s;" % csp_hash(styles[0]), policy)
    test.assertIn("default-src 'none'", policy)
    # Text holds no markup, so every '<' begins one of the renderer's own elements.
    tags = re.findall(r"<([a-zA-Z][a-zA-Z0-9]*)([^>]*)>", page)
    allowed = TAGS | ({"img", "rect"} if images else set())
    test.assertTrue(set(name for name, _ in tags) <= allowed,
                    set(name for name, _ in tags) - allowed)
    for name, attributes in tags:
        for attribute, value in re.findall(r'\s([a-zA-Z:-]+)="([^"]*)"', attributes):
            if name == "img" and attribute == "src":
                test.assertFalse(value.startswith(("/", ".", "#")) or ":" in value, value)
                test.assertTrue(value.endswith(".svg") and os.path.isfile(
                    os.path.join(output, *value.split("/"))), value)
                continue
            test.assertNotRegex(attribute, r"^(on|style$|src$|srcset$|action$)", name)
            test.assertNotIn("javascript:", value)
            if attribute == "href":
                test.assertFalse(value.startswith(("/", ".", "#")) or ":" in value, value)
                test.assertTrue(os.path.isfile(os.path.join(output, *value.split("/"))),
                                value)
            elif attribute == "xmlns":
                test.assertEqual(value, "http://www.w3.org/2000/svg")
            else:
                test.assertNotRegex(value, r"https?:", attribute)
    for name, attributes in tags:
        for attribute, value in re.findall(r'\s([a-zA-Z:-]+)="([^"]*)"', attributes):
            if (name, attribute) in (("a", "href"), ("img", "src")) and value.endswith(".svg"):
                assert_script_free_svg(test, read(os.path.join(output, *value.split("/"))),
                                       value)


def assert_script_free_svg(test, data, where):
    """A linked or embedded SVG is a view only when it holds no script, event attribute or
    external reference, and the allowlist sanitizer would remove nothing from it (ADR-0179):
    an SVG opened on its own is outside the linking page's CSP."""
    test.assertNotRegex(data, rb"(?i)<script|\son[a-z]+\s*=|javascript:|href\s*=\s*[\"']?https?:",
                        where)
    again = svgsafe.sanitize(data)
    test.assertEqual((again.data, again.removed_elements, again.removed_attributes),
                     (data, 0, 0), where)


class Reports(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.suite = synthetic_suite()

    def write(self, plan, value, observe=False, **options):
        run = run_dir(self, plan, value, **options)
        if observe:
            # Measured on the activation's calibrated CPU/image group (ADR-0184).
            synthetic.observe(run, plan)
        output = self.path("report-%d" % len(os.listdir(self.tmp)))
        return run, output, report.write(run, output)

    def page(self, output):
        return read(os.path.join(output, report.REPORT_HTML)).decode("utf-8")

    def assert_safe_page(self, page, output):
        assert_safe_page(self, page, output)

    def test_a_report_shows_every_planned_slot_and_the_verdict_s_exact_bounds(self):
        plan = synthetic.plan(self.suite)

        def value(slot, case):
            # A candidate twice as slow on one case: a regression the verdict classifies.
            slow = case["instance_id"] == "synthetic_fast" and slot["side"] == "candidate"
            return Fraction(21, 2) * (2 if slow else 1)

        run, output, doc = self.write(plan, value)
        verdict = compare.compare(run)
        self.assertEqual(doc["verdict"], verdict)
        self.assertEqual(json.loads(read(os.path.join(output, report.VERDICT_JSON))), verdict)
        self.assertEqual(report.read(os.path.join(output, report.REPORT_JSON)), doc)
        # Every planned slot, sentinels and warmups included, in planned order.
        for shard, entry in zip(doc["sequences"], plan["shards"]):
            slots = schedule.expected_slots(plan, entry["index"])
            self.assertEqual([(p["sequence"], p["phase"], p["instance_id"], p["side"])
                              for p in shard["points"]],
                             [(s["sequence"], s["phase"], s["instance_id"], s["side"])
                              for s in slots])
            phases = set(point["phase"] for point in shard["points"])
            self.assertEqual(phases, {"sentinel_start", "warmup", "measured", "sentinel_end"})
        page = self.page(output)
        regressed = [case for case in verdict["cases"] if case["instance_id"] == "synthetic_fast"]
        self.assertEqual(regressed[0]["status"], "regression")
        interval = regressed[0]["interval"]
        self.assertIn("[%s" % interval["lower"], page)
        self.assertIn(interval["upper"], page)
        self.assertIn(verdict["algorithm"], page)
        self.assertIn(plan["authority"]["policy_digest"], page)
        self.assertIn(plan["profile"]["recipe_digest"], page)
        self.assertIn(plan["profile"]["environment_digest"], page)
        self.assert_safe_page(page, output)
        summary = read(os.path.join(output, report.SUMMARY_MD)).decode()
        self.assertIn("| synthetic&#95;fast | regression | fail |", summary)

    def test_wording_follows_the_evidence_and_the_verdict_leads_with_affected_cases(self):
        # A synthetic fixture never claims that pairs ran on a host or that values were
        # measured; a measured run says only what its retained records show.
        plan = synthetic.plan(self.suite)

        def value(slot, case):
            slow = case["instance_id"] == "synthetic_fast" and slot["side"] == "candidate"
            return Fraction(10) * (3 if slow else 1)

        for provenance in ("synthetic", "measured"):
            with self.subTest(provenance=provenance):
                run, output, doc = self.write(plan, value, observe=provenance == "measured",
                                              provenance=provenance)
                page = self.page(output)
                summary = read(os.path.join(output, report.SUMMARY_MD)).decode()
                for text in (page, summary, doc["sensitivity"]["detail"]):
                    if provenance == "synthetic":
                        self.assertNotIn("on one host", text)
                        self.assertNotIn("These are measured results", text)
                    else:
                        self.assertNotIn("constructed", text)
                if provenance == "synthetic":
                    self.assertIn("Synthetic evidence: constructed fixture values, not "
                                  "measurements", page)
                    self.assertIn("Synthetic evidence", summary)
                    self.assertIn("These are constructed values", doc["sensitivity"]["detail"])
                else:
                    self.assertIn("Measured evidence", page)
                    self.assertIn("on one host", page)
                    self.assertIn("These are measured results", doc["sensitivity"]["detail"])
                # The first section is the verdict: its result, the affected case with its
                # baseline, candidate, absolute and relative change, and every eligibility
                # reason as its own item; the exact statistics and provenance stay below.
                sections = re.findall(r"<section><h2>(.*?)</h2>", page)
                self.assertEqual(sections[:3], ["Verdict", "Cases", "Statistics"])
                first = page[page.index("<section>"):page.index("</section>")]
                self.assertIn("fail (exit 3)", first)
                self.assertIn("1 of %d cases failed or incomplete" % len(plan["cases"]), first)
                row = re.search(r"<tr><td>synthetic_fast</td>.*?</tr>", first).group(0)
                for cell in ("regression", "<td>10</td>", "<td>30</td>", "<td>20 ms</td>",
                             "<td>200%</td>"):
                    self.assertIn(cell, row)
                self.assertNotIn("synthetic_slow", first)
                items = re.findall(r"<li>(.*?)</li>", first)
                self.assertEqual(len(items), len(doc["required_check"]["reasons"]))
                self.assertIn("<details><summary>Exact intervals", page)
                interval = [case for case in doc["verdict"]["cases"]
                            if case["instance_id"] == "synthetic_fast"][0]["interval"]
                self.assertIn(interval["lower"], page[page.index("<details>"):])
                self.assert_safe_page(page, output)

    def test_only_failed_or_incomplete_cases_are_affected_and_passes_are_never_problems(self):
        # A regression beside a passing improvement and a passing non-regression: the first
        # view names only the regression; the improvement and the non-regression stay in the
        # Cases table and the summary as passes (ADR-0187). A run without a failure says every
        # case passed, by status, never that an improvement is within tolerance.
        plan = synthetic.plan(self.suite)
        spread = [-3, -3, -2, -1, 0, 0, 0, 0, 0, "0.5", 1, 1]

        def value(regressed):
            def at(slot, case):
                if slot["side"] != "candidate" or slot["phase"] != "measured":
                    return Fraction(10)
                if case["instance_id"] == "synthetic_fast":
                    return Fraction(30 if regressed else 10)
                if case["instance_id"] == "synthetic_process__size_1":
                    return Fraction(5)
                return Fraction(10) + Fraction(spread[slot["pair"] - 1])
            return at

        for regressed in (True, False):
            with self.subTest(regressed=regressed):
                run, output, doc = self.write(plan, value(regressed), observe=True,
                                              provenance="measured")
                statuses = dict((entry["instance_id"], (entry["status"], entry["outcome"]))
                                for entry in doc["verdict"]["cases"])
                self.assertEqual(statuses["synthetic_process__size_1"], ("improvement", "pass"))
                self.assertEqual(statuses["synthetic_advisory"], ("non_regression", "pass"))
                self.assertEqual(statuses["synthetic_fast"],
                                 ("regression", "fail") if regressed
                                 else ("within_tolerance", "pass"))
                page = self.page(output)
                first = page[page.index("<section>"):page.index("</section>")]
                cases = page[page.index("<h2>Cases</h2>"):]
                cases = cases[:cases.index("</section>")]
                summary = read(os.path.join(output, report.SUMMARY_MD)).decode()
                for instance in ("synthetic_process__size_1", "synthetic_advisory"):
                    self.assertNotIn(instance, first)
                    self.assertRegex(cases, r"<td>%s</td><td>[^<]*</td><td><span class=\"pass"
                                     r"\">pass</span></td>" % instance)
                self.assertIn("| synthetic&#95;process&#95;&#95;size&#95;1 | improvement | pass |",
                              summary)
                self.assertIn("| synthetic&#95;advisory | non&#95;regression | pass |", summary)
                self.assertNotIn("within tolerance.", page)
                if regressed:
                    self.assertIn("1 of 3 cases failed or incomplete", first)
                    self.assertIn("<td>synthetic_fast</td>", first)
                    self.assertIn("| synthetic&#95;fast | regression | fail |", summary)
                else:
                    self.assertIn("0 of 3 cases failed or incomplete", first)
                    self.assertIn("No case failed or is incomplete: every case passed (1 "
                                  "improvement, 1 non regression, 1 within tolerance)", first)
                    self.assertNotIn("<table>", first)
                self.assert_safe_page(page, output)

    def test_incomplete_or_failed_measured_runs_never_claim_every_pair_ran(self):
        # Measured shards with a missing planned record, or with failed samples, keep measured
        # provenance; the page and the summary state what was admitted and what is missing or
        # failed, never that each pair's samples were recorded (ADR-0187).
        plan = synthetic.plan(self.suite)
        case = [item for item in plan["cases"] if item["instance_id"] == "synthetic_fast"][0]
        slots = [slot for slot in schedule.expected_slots(plan, case["shard"])
                 if slot["instance_id"] == "synthetic_fast" and slot["phase"] == "measured"]
        claim = "each pair&#x27;s baseline and candidate samples were recorded"
        for label, options, missing, failed in (
                ("missing", {"omit": [schedule.slot_key(slots[0])],
                             "shard_status": {case["shard"]: "interrupted"}}, 1, 0),
                ("failed", {"failures": dict((schedule.slot_key(slot), "oracle_mismatch")
                                             for slot in [slot for slot in slots
                                                          if slot["side"] == "candidate"][:2])},
                 0, 2),
                ("unfinished", {"shard_status": {case["shard"]: "interrupted"}}, 0, 0),
                ("complete", {}, 0, 0)):
            with self.subTest(label=label):
                run, output, doc = self.write(plan, synthetic.constant("10"), observe=True,
                                              provenance="measured", **options)
                self.assertEqual(doc["verdict"]["provenance"], "measured")
                coverage = doc["verdict"]["aggregate"]["coverage"]
                self.assertEqual(coverage["missing_records"], missing)
                page = self.page(output)
                summary = read(os.path.join(output, report.SUMMARY_MD)).decode()
                first = page[page.index("<section>"):page.index("</section>")]
                if label == "complete":
                    self.assertIn(claim, first)
                    self.assertIn("every planned record was admitted and succeeded", summary)
                    continue
                self.assertNotIn(claim, first)
                for text in (html.unescape(first), summary.replace("&#95;", "_")):
                    self.assertNotIn("each pair's baseline and candidate samples", text)
                    self.assertNotIn("every planned record was admitted", text)
                    self.assertIn("Measured evidence, incomplete", text)
                    self.assertIn("Of %d planned records, %d were admitted; missing: %d; "
                                  "failed samples among the admitted: %d"
                                  % (coverage["expected_records"], coverage["matched_records"],
                                     missing, failed), text)
                    if label == "unfinished":
                        self.assertIn("shards that did not complete: %d" % case["shard"], text)
                        self.assertIn("Every planned record was admitted and succeeded, but a "
                                      "shard's terminal record says it did not complete", text)
                        self.assertNotIn("Not every planned pair", text)
                    else:
                        self.assertIn("Not every planned pair was recorded and succeeded", text)
                self.assertIn("synthetic_fast", first)
                self.assert_safe_page(page, output)

    def test_failures_are_side_specific_and_missing_records_keep_the_denominator(self):
        plan = synthetic.plan(self.suite)
        case = [item for item in plan["cases"] if item["instance_id"] == "synthetic_fast"][0]
        slots = [slot for slot in schedule.expected_slots(plan, case["shard"])
                 if slot["instance_id"] == "synthetic_fast" and slot["phase"] == "measured"]
        failed = [slot for slot in slots if slot["side"] == "candidate"][:2]
        missing = [slot for slot in slots if slot["side"] == "baseline"][:1]
        run, output, doc = self.write(
            plan, synthetic.constant("10"),
            failures=dict((schedule.slot_key(slot), "oracle_mismatch") for slot in failed),
            omit=[schedule.slot_key(slot) for slot in missing],
            shard_status={case["shard"]: "interrupted"})
        extras = dict((item["instance_id"], item) for item in doc["cases"])
        self.assertEqual(extras["synthetic_fast"]["failures"], {"baseline": 0, "candidate": 2})
        denominator = extras["synthetic_fast"]["denominator"]
        self.assertEqual(denominator["missing_records"], 1)
        self.assertEqual(denominator["planned_records"],
                         2 * (case["warmup_pairs"] + case["pairs"]))
        points = [point for shard in doc["sequences"] for point in shard["points"]
                  if point["instance_id"] == "synthetic_fast"]
        self.assertEqual(sorted((p["side"], p["status"], p["failure"]) for p in points
                                if p["status"] != "ok"),
                         [("baseline", "missing", None),
                          ("candidate", "failed", "oracle_mismatch"),
                          ("candidate", "failed", "oracle_mismatch")])
        self.assertEqual(doc["verdict"]["aggregate"]["result"], "incomplete")
        self.assertFalse(doc["required_check"]["satisfied"])
        self.assertIn("result_not_pass", doc["required_check"]["reasons"])
        page = self.page(output)
        self.assertIn("1 / %d" % denominator["planned_records"], page)
        self.assertIn("0 / 2", page)
        self.assert_safe_page(page, output)

    def test_no_report_turns_bootstrap_advisory_or_synthetic_evidence_into_a_passing_check(self):
        for scope, mode, provenance in (("bootstrap", "control", "synthetic"),
                                        ("full", "active", "synthetic"),
                                        ("partial_scope", "control", "measured"),
                                        ("full", "active", "measured")):
            with self.subTest(scope=scope, provenance=provenance):
                plan = synthetic.plan(self.suite, scope=scope, mode=mode,
                                      cases=["synthetic_fast"] if scope == "partial_scope"
                                      else None)
                run, output, doc = self.write(plan, synthetic.constant("10.5"),
                                              observe=provenance == "measured",
                                              provenance=provenance)
                verdict = doc["verdict"]
                self.assertEqual(verdict["aggregate"]["result"], "pass")
                check = doc["required_check"]
                expected = [reason for reason, holds in (
                    ("scope_not_full", scope != "full"),
                    ("policy_not_active", mode != "active"),
                    ("evidence_not_measured", provenance != "measured")) if holds]
                self.assertEqual(check["reasons"], expected)
                self.assertEqual(check["satisfied"], not expected)
                self.assertEqual(check["satisfied"],
                                 verdict["aggregate"]["satisfies_required_check"])
                page = self.page(output)
                summary = read(os.path.join(output, report.SUMMARY_MD)).decode()
                if expected:
                    self.assertIn("does not satisfy the required check", page)
                    self.assertNotIn("; satisfies the required check", page)
                    self.assertIn("does not satisfy the required check", summary)
                else:
                    self.assertIn("; satisfies the required check", page)
                # Measured results are kept apart from calibrated sensitivity.
                self.assertIn("no minimum detectable effect is inferred", page)
                self.assertEqual(doc["sensitivity"]["state"],
                                 "calibrated_admission" if mode == "active"
                                 else "not_calibrated")
                self.assertEqual(doc["comparison"], "same_host_paired")
                for item in doc["cases"]:
                    self.assertEqual(item["profile"]["state"], "unavailable")
                    self.assertEqual(item["trace"]["state"], "unavailable")
                self.assertIn("separate diagnostic child runs", page)

    def test_every_evidence_string_is_escaped_and_no_active_content_survives(self):
        plan = synthetic.plan(self.suite)
        case = [item for item in plan["cases"] if item["instance_id"] == "synthetic_fast"][0]
        failed = [slot for slot in schedule.expected_slots(plan, case["shard"])
                  if slot["instance_id"] == "synthetic_fast" and slot["side"] == "candidate"
                  and slot["phase"] == "measured"][0]

        def hostile(record):
            if record["failure"] is not None:
                record["failure"]["detail"] = INJECTION
            return [record]

        run = run_dir(self, plan, synthetic.constant("10"), edit=hostile,
                      failures={schedule.slot_key(failed): "oracle_mismatch"},
                      shard_status={1 - case["shard"]: "interrupted"})
        other = os.path.join(run, "shards", str(1 - case["shard"]))

        def detail(doc):
            doc["failure"] = {"reason": "synthetic_interruption", "detail": INJECTION}
        synthetic.reseal_shard(other, detail)
        output = self.path("hostile-report")
        doc = report.write(run, output)
        page = self.page(output)
        self.assertNotIn("<script>alert", page)
        self.assertNotIn("<img", page)
        self.assertNotIn("<!--", page)
        self.assertIn("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;&quot;&gt;&lt;img src=x "
                      "onerror=alert(2)&gt;", page)
        self.assert_safe_page(page, output)
        # The same strings are data in the JSON and cells in the job summary.
        self.assertIn(INJECTION, [point["detail"] for shard in doc["sequences"]
                                  for point in shard["points"]])
        summary = read(os.path.join(output, report.SUMMARY_MD)).decode()
        self.assertNotIn("<", summary)
        for line in summary.splitlines():
            if line.startswith("| "):
                self.assertEqual(line.count("|"), 10, line)
        headline = render.markdown_cell(INJECTION)
        self.assertNotIn("<", headline)
        self.assertNotIn("|", headline)
        self.assertNotIn("`", headline)
        self.assertNotIn("[", headline)

    def test_links_name_only_validated_files_below_the_report(self):
        for bad in ("https://example.invalid/x", "/etc/passwd", "../outside", "evidence/../x",
                    ".hidden", "a/.b", "a//b", "a\\b", "javascript:alert(1)"):
            with self.assertRaises(ContractError, msg=bad):
                render.link(bad, "x")
        self.assertEqual(render.link("evidence/shards/0/samples.jsonl", "<x>"),
                         '<a href="evidence/shards/0/samples.jsonl" download>&lt;x&gt;</a>')

    def test_the_output_is_new_and_never_inside_the_run(self):
        plan = synthetic.plan(self.suite)
        run = run_dir(self, plan, synthetic.constant("10"))
        with self.assertRaises(ContractError) as caught:
            report.write(run, os.path.join(run, "shards", "report"))
        self.assertIn("inside the run directory", str(caught.exception))
        occupied = self.path("occupied")
        os.mkdir(occupied)
        with open(os.path.join(occupied, "keep"), "w") as stream:
            stream.write("mine")
        with self.assertRaises(ContractError):
            report.write(run, occupied)
        self.assertEqual(read(os.path.join(occupied, "keep")), b"mine")
        # The run itself is untouched, so it still verifies whole.
        self.assertEqual(compare.compare(run)["aggregate"]["coverage"]["missing_records"], 0)

    def test_a_tampered_or_partial_run_is_reported_as_incomplete_never_repaired(self):
        plan = synthetic.plan(self.suite)
        run = run_dir(self, plan, synthetic.constant("10"))
        samples = os.path.join(run, "shards", "0", "samples.jsonl")
        with open(samples, "ab") as stream:
            stream.write(b"\n")
        output = self.path("tampered")
        doc = report.write(run, output)
        self.assertEqual(doc["verdict"]["aggregate"]["result"], "incomplete")
        self.assertIsNone(doc["shards"][0]["terminal"])
        self.assertTrue(all(point["status"] == "missing"
                            for point in doc["sequences"][0]["points"]))
        # Only verified bundles were copied as downloads.
        paths = [item["path"] for item in doc["downloads"]]
        self.assertFalse(any(path.startswith("evidence/shards/0/") for path in paths))
        self.assertIn("evidence/shards/1/samples.jsonl", paths)
        self.assertIn("shard_bundle_invalid", self.page(output))

    def test_the_report_contract_refuses_inconsistent_documents(self):
        plan = synthetic.plan(self.suite)
        _, output, doc = self.write(plan, synthetic.constant("10"))
        for change, why in (
                (lambda d: d["required_check"].update(satisfied=True, reasons=[]),
                 "satisfied exactly when the verdict satisfies it"),
                (lambda d: d["cases"].reverse(), "one entry per verdict case"),
                (lambda d: d["sequences"][0]["points"][3].update(value=None),
                 "a value exactly when ok"),
                (lambda d: d["downloads"].reverse(), "sorted, each path once")):
            edited = json.loads(json.dumps(doc))
            change(edited)
            with self.assertRaises(ContractError) as caught:
                contracts.validate_report(edited)
            self.assertIn(why, str(caught.exception))

    def test_the_command_writes_the_report_and_exits_with_the_verdict_s_code(self):
        plan = synthetic.plan(self.suite)
        run = run_dir(self, plan, synthetic.constant("10"))
        summary = self.path("step-summary.md")
        with open(summary, "w") as stream:
            stream.write("earlier step\n")
        result = subprocess.run([sys.executable, CLI, "report", "--run", run, "--output",
                                 self.path("cli-report"), "--job-summary", summary],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.decode().strip().endswith("report.html"))
        self.assertIn(b"does not satisfy the required check", result.stderr)
        text = read(summary).decode()
        self.assertTrue(text.startswith("earlier step\n### AF performance: pass"))
        refused = subprocess.run([sys.executable, CLI, "report", "--run", self.path("nothing"),
                                  "--output", self.path("cli-report-2")],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(refused.returncode, 2)
