"""Telemetry captures of diagnostic child runs (ADR-0199).

`fixtures/performance/telemetry-diagnostic/` holds the four `af telemetry` documents a debug
build of AF 0.12.0-rc.5 wrote for the CLI Task fixture of `cli_task_run_check` in a private HOME
(its directory path replaced by `/scratch/sample`). The documents are read here only through the
product's committed schemas, then mutated into every partial and hostile state a capture must
never present as complete, as escaped text, or outside its evidence. The child runs use the
stand-in AF, supervisor and container runtime of `tests/diagnostic_fixtures.py`; nothing here
measures anything or makes a performance claim.
"""
import json
import os
from unittest import mock

from perflab import (bundle, contracts, diagcontracts, diagnostics, jsonsubset, oracle,
                     phasetrace, planning, report, telemetrydiag)
from perflab.values import ContractError
from tests import diagnostic_fixtures as fakes
from tests.support import ROOT, SUITE, TempDir, fake_supervisor, read, writable_copy
from tests.test_diagnostics import DISCOVERY, SMALL, ChildRunBase

DOCUMENTS = os.path.join(ROOT, "fixtures/performance/telemetry-diagnostic")
TASK_RUN = "cli_task_run_check__task_cli_task_roots"
TASK = "perf-check-true"


def documents():
    return dict((kind, read(os.path.join(DOCUMENTS, kind + ".json")))
                for kind in telemetrydiag.DOCUMENT_KINDS)


def loaded():
    return dict((kind, json.loads(data.decode("utf-8"))) for kind, data in documents().items())


def dumped(docs):
    return dict((kind, (json.dumps(doc, indent=2) + "\n").encode("utf-8"))
                for kind, doc in docs.items())


def inventory_cases():
    """Mutations of the current writer's full list, sealed ordinals and open objects."""
    for source in ("runs", "show"):
        for mutation in ("retained_low", "retained_high", "sealed_missing", "sealed_extra",
                         "open_extra", "open_missing", "open_ordinal_missing",
                         "open_ordinal_extra", "only_here"):
            docs = loaded()
            run = docs[source]["runs"][0] if source == "runs" else docs[source]["run"]
            inventory = run["executions"]
            if mutation == "retained_low":
                inventory["retained"] = 0
            elif mutation == "retained_high":
                inventory["retained"] = 2
            elif mutation == "sealed_missing":
                inventory["sealed"] = []
            elif mutation == "sealed_extra":
                inventory["sealed"].append(2)
            elif mutation == "open_extra":
                inventory["open"] = [{"execution_id": "extra", "ordinal": 2, "segment": "live"}]
            else:
                extra = dict(inventory["list"][0], execution_id="extra", ordinal=2,
                             span_id="e" * 16)
                inventory["list"].append(extra)
                inventory["retained"] = 2
                if mutation == "only_here":
                    inventory["sealed"] = [1, 2]
                    run.update(history="prefix:2", retained="retained executions 1–2")
                    if source == "runs":
                        run["execution_us"] += extra["wall_us"]
                        run["totals_cover"].append({"execution_id": "extra", "ordinal": 2,
                                                    "open": False})
                    if source == "show":
                        entry = dict(docs["status"]["bytes"]["entries"][0], name="extra")
                        docs["status"]["bytes"]["entries"].append(entry)
                        docs["status"]["segments"]["sealed"] = 2
                        docs["show"]["spans"].append({
                            "span_id": extra["span_id"], "name": "af.run", "depth": 0,
                            "segment_id": "extra", "start_offset_us": 0,
                            "total_us": extra["wall_us"], "self_us": extra["wall_us"],
                            "status": "ok", "attributes": {}})
                else:
                    extra.update(segment="live", open=True)
                    opened = {"execution_id": "extra", "ordinal": 2, "segment": "live"}
                    inventory["open"] = [] if mutation == "open_missing" else [opened]
                    if mutation == "open_ordinal_missing":
                        del opened["ordinal"]
                    elif mutation == "open_ordinal_extra":
                        extra["ordinal"] = None
            yield source + ":" + mutation, docs, mutation != "only_here"


def identity_cases():
    """A valid partial observation and a contradiction with identical derived figures.

    Only the execution inventory's immutable span binding changes, so neither summary
    disagreement nor a stale rendered page/hash can stand in for reverse reconciliation.
    """
    valid = loaded()
    item = valid["runs"]["runs"][0]["executions"]["list"][0]
    original_span = item["span_id"]
    item.update(execution_id="replacement", span_id="e" * 16)
    valid["runs"]["runs"][0]["totals_cover"][0]["execution_id"] = "replacement"
    invalid = json.loads(json.dumps(valid))
    invalid["runs"]["runs"][0]["executions"]["list"][0]["span_id"] = original_span
    yield "cross_reader_reassignment", valid, invalid
    for source in ("runs", "show"):
        _, valid, _ = next(case for case in inventory_cases()
                           if case[0] == source + ":only_here")
        invalid = json.loads(json.dumps(valid))
        run = invalid[source]["runs"][0] if source == "runs" else invalid[source]["run"]
        run["executions"]["list"][1]["span_id"] = run["executions"]["list"][0]["span_id"]
        yield source + ":duplicate_span", valid, invalid


class Documents(TempDir):
    def test_reverse_execution_identity_refuses_reassignment_and_duplicate_spans(self):
        for name, valid, invalid in identity_cases():
            with self.subTest(mutation=name):
                derived = telemetrydiag.capture(dumped(valid), TASK)
                self.assertEqual(derived["evidence"]["state"], "partial")
                self.assertIn("history_partial", derived["evidence"]["reasons"])
                with self.assertRaisesRegex(ContractError, "span identity"):
                    telemetrydiag.capture(dumped(invalid), TASK)

    def test_execution_summaries_and_both_reader_inventories_are_reconciled(self):
        for name, docs, refused in inventory_cases():
            with self.subTest(mutation=name):
                if refused:
                    with self.assertRaises(ContractError):
                        telemetrydiag.capture(dumped(docs), TASK)
                else:
                    derived = telemetrydiag.capture(dumped(docs), TASK)
                    self.assertEqual(derived["evidence"]["state"], "partial")
                    self.assertIn("history_partial", derived["evidence"]["reasons"])

    def test_valid_nullable_open_recovered_and_terminal_execution_summaries(self):
        for segment, opened, ordinal in (("live", False, 1), ("unsealed", False, None),
                                         ("sealed", True, 1), ("sealed", False, None),
                                         ("sealed", False, 1)):
            with self.subTest(segment=segment, opened=opened, ordinal=ordinal):
                docs = loaded()
                for run in (docs["runs"]["runs"][0], docs["show"]["run"]):
                    inventory = run["executions"]
                    item = inventory["list"][0]
                    item.update(segment=segment, open=opened, ordinal=ordinal)
                    is_open = opened or segment != "sealed"
                    inventory["sealed"] = [ordinal] if not is_open and ordinal is not None else []
                    entry = {"execution_id": item["execution_id"], "segment": segment}
                    if ordinal is not None:
                        entry["ordinal"] = ordinal
                    inventory["open"] = [entry] if is_open else []
                    if ordinal is None:
                        run["history"] = "partial"
                docs["status"]["segments"].update(sealed=0, live=0, unsealed=0)
                docs["status"]["segments"][segment] = 1
                docs["status"]["bytes"]["entries"][0]["state"] = segment
                derived = telemetrydiag.capture(dumped(docs), TASK)
                self.assertEqual(derived["evidence"]["state"], "complete" if
                                 segment == "sealed" and not opened and ordinal == 1 else "partial")

    def test_the_recorded_documents_give_a_complete_capture_of_their_one_run(self):
        derived = telemetrydiag.capture(documents(), TASK)
        self.assertEqual(derived["evidence"]["state"], "complete")
        self.assertEqual(derived["evidence"]["reasons"], [])
        self.assertEqual((derived["run"]["kind"], derived["run"]["task_id"]), ("task", TASK))
        self.assertEqual(len(derived["executions"]), 1)
        self.assertEqual(derived["spans"], 68)
        self.assertEqual(sum(row["spans"] for row in derived["phases"]), 68)
        names = [row["name"] for row in derived["phases"]]
        self.assertEqual(names, sorted(names))
        self.assertIn("af.store.append", names)
        # Covered time is the union of a name's spans, never more than the command's own.
        command = [row for row in derived["phases"] if row["name"] == "af.command"][0]
        for row in derived["phases"]:
            self.assertLessEqual(row["covered_us"], command["covered_us"])
            self.assertLessEqual(row["longest_us"], row["covered_us"])
        self.assertEqual(derived["af"]["version"], "0.12.0-rc.5")

    def test_missing_wrong_duplicate_and_conflicting_segment_evidence(self):
        for mutation in ("missing", "wrong_execution", "missing_metadata", "unknown_state",
                         "duplicate", "mismatched_state", "sealed_count", "duplicate_execution"):
            with self.subTest(mutation=mutation):
                docs = loaded()
                entry = docs["status"]["bytes"]["entries"][0]
                if mutation == "missing":
                    docs["status"]["bytes"]["entries"].pop(0)
                elif mutation == "wrong_execution":
                    entry["name"] = "another-execution"
                elif mutation == "missing_metadata":
                    entry.pop("capture")
                    entry.pop("exit")
                elif mutation == "unknown_state":
                    entry.pop("state")
                elif mutation == "duplicate":
                    docs["status"]["bytes"]["entries"].append(dict(entry))
                elif mutation == "mismatched_state":
                    entry["state"] = "live"
                    docs["status"]["segments"].update(sealed=0, live=1)
                elif mutation == "sealed_count":
                    docs["status"]["segments"]["sealed"] = 0
                else:
                    docs["show"]["run"]["executions"]["list"].append(
                        dict(docs["show"]["run"]["executions"]["list"][0]))
                if mutation in ("duplicate", "mismatched_state", "sealed_count", "duplicate_execution"):
                    with self.assertRaises(ContractError):
                        telemetrydiag.capture(dumped(docs), TASK)
                else:
                    evidence = telemetrydiag.capture(dumped(docs), TASK)["evidence"]
                    self.assertEqual(evidence["state"], "partial")
                    self.assertTrue(evidence["reasons"])
                    self.assertEqual(len(evidence["segment_captures"]), 1)

    def test_the_perfetto_export_nests_valid_tracks_and_copies_no_attribute(self):
        data = telemetrydiag.perfetto(documents()["show"], "case side")
        doc = json.loads(data.decode("utf-8"))
        events = doc["traceEvents"]
        complete = [event for event in events if event["ph"] == "X"]
        self.assertEqual(len(complete) + len([e for e in events if e["ph"] == "b"]), 68)
        for event in complete:
            self.assertEqual(sorted(event["args"]), ["parent", "self_us", "span_id", "status"])
        # Every nested complete event lies inside the one before it that encloses it.
        stack = []
        for event in sorted(complete, key=lambda item: (item["ts"], -item["dur"])):
            while stack and stack[-1][1] <= event["ts"]:
                stack.pop()
            if stack:
                self.assertLessEqual(event["ts"] + event["dur"], stack[-1][1])
            stack.append((event["ts"], event["ts"] + event["dur"]))
        self.assertNotIn(b"af.policy_rev", data)
        self.assertEqual(data, telemetrydiag.perfetto(documents()["show"], "case side"))

    def test_open_unsealed_dropped_recovered_and_incomplete_evidence_is_partial(self):
        cases = []

        def open_span(docs):
            span = docs["show"]["spans"][3]
            span["status"] = "open"
            del span["total_us"], span["self_us"]
        cases.append((open_span, "open_spans"))

        def unsealed(docs):
            docs["status"]["segments"]["unsealed"] = 1
        cases.append((unsealed, "unsealed_segment"))

        def recovered(docs):
            for entry in docs["status"]["bytes"]["entries"]:
                if entry["kind"] == "segment":
                    entry["exit"], entry["capture"] = "recovered", "recovered, drops unknown"
        cases.append((recovered, "dropped_or_recovered_capture"))

        def dropped(docs):
            docs["runs"]["runs"][0]["coverage"] = "incomplete"
        cases.append((dropped, "dropped_or_recovered_capture"))

        def config(docs):
            docs["config"]["state"] = "incomplete"
        cases.append((config, "config_incomplete"))

        def history(docs):
            docs["show"]["run"]["history"] = "partial"
        cases.append((history, "history_partial"))

        def unreadable(docs):
            docs["show"]["unreadable"] = [{"path": "v1/segments/x.jsonl", "reason": "corrupt"}]
        cases.append((unreadable, "unreadable_files"))

        def problem(docs):
            docs["status"]["problems"] = [{"what": "write", "reason": "quota", "segments": 1}]
        cases.append((problem, "problems_recorded"))

        def live(docs):
            execution = docs["show"]["run"]["executions"]["list"][0]
            execution["segment"], execution["open"] = "live", True
            docs["show"]["run"]["executions"].update(sealed=[], open=[{
                "execution_id": execution["execution_id"], "ordinal": execution["ordinal"],
                "segment": "live"}])
            docs["status"]["segments"].update(sealed=0, live=1)
            entry = docs["status"]["bytes"]["entries"][0]
            entry["state"] = "live"
            entry.pop("exit", None)
            entry.pop("capture", None)
        cases.append((live, "open_execution"))
        for mutate, reason in cases:
            with self.subTest(reason=reason):
                docs = loaded()
                mutate(docs)
                derived = telemetrydiag.capture(dumped(docs), TASK)
                self.assertEqual(derived["evidence"]["state"], "partial")
                self.assertIn(reason, derived["evidence"]["reasons"])

    def test_documents_the_product_schema_refuses_or_that_disagree_are_never_read(self):
        def unknown_field(docs):
            docs["show"]["spans"][0]["note"] = "x"

        def another_version(docs):
            docs["show"]["schema"] = "af/telemetry-show@2"

        def two_runs(docs):
            docs["runs"]["runs"].append(dict(docs["runs"]["runs"][0], run_id="task:other"))

        def review_run(docs):
            docs["runs"]["runs"][0]["kind"] = "review"

        def other_task(docs):
            docs["runs"]["runs"][0]["task_id"] = "somebody-else"

        def other_store(docs):
            docs["show"]["run"]["store_id"] = "0" * 64

        def stray_segment(docs):
            docs["show"]["spans"][5]["segment_id"] = "another-execution"

        def lost_parent(docs):
            docs["show"]["spans"][5]["parent_span_id"] = "0" * 16

        def long_name(docs):
            docs["show"]["spans"][5]["name"] = "a" * 200

        def self_over_total(docs):
            docs["show"]["spans"][5]["self_us"] = docs["show"]["spans"][5]["total_us"] + 1
        for mutate in (unknown_field, another_version, two_runs, review_run, other_task,
                       other_store, stray_segment, lost_parent, long_name, self_over_total):
            with self.subTest(mutation=mutate.__name__):
                docs = loaded()
                mutate(docs)
                with self.assertRaises(ContractError):
                    telemetrydiag.capture(dumped(docs), TASK)
        for data in (b'{"schema": "af/telemetry-runs@1", "schema": "x"}', b"\xff\xfe",
                     b'{"a": NaN}', b"x" * (telemetrydiag.DOCUMENT_BYTES_MAX + 1)):
            with self.subTest(data=data[:16]):
                with self.assertRaises(ContractError):
                    telemetrydiag.read("runs", data, "runs.json")

    def test_the_schema_evaluator_refuses_keywords_it_does_not_implement(self):
        with self.assertRaises(ContractError) as caught:
            jsonsubset.Schema({"type": "object", "if": {}}, "made-up.json")
        self.assertIn("does not implement: if", str(caught.exception))
        schema = jsonsubset.Schema({"type": "object", "additionalProperties": False,
                                    "properties": {"n": {"type": ["integer", "null"],
                                                         "exclusiveMinimum": 0}}}, "s")
        self.assertIsNone(schema.errors({"n": 1}))
        self.assertIsNone(schema.errors({"n": None}))
        self.assertIsNotNone(schema.errors({"n": 0}))
        self.assertIsNotNone(schema.errors({"n": True}))
        self.assertIsNotNone(schema.errors({"m": 1}))

    def test_untrusted_names_are_escaped_on_the_page(self):
        docs = loaded()
        docs["show"]["spans"][5]["name"] = '<script>alert("x")</script>'
        derived = telemetrydiag.capture(dumped(docs), TASK)
        entry = dict(derived, instance_id=TASK_RUN, side="baseline", subject="cli_process",
                     level="spans", source_sha="a" * 40, binary_sha256="sha256:" + "b" * 64,
                     command_wall_ms="12.5", artifacts=[], omission=None)
        html = telemetrydiag.section({"level": "spans", "perturbing": False,
                                      "verdict_effect": "none", "captures": [entry]})
        self.assertNotIn("<script>", html)
        self.assertIn("&lt;script&gt;", html)

    def test_the_supported_matrix_names_every_case_that_records_no_telemetry(self):
        self.assertEqual(telemetrydiag.eligibility({"kind": "library", "case_id": "cas",
                                                    "instance_id": SMALL})[0],
                         "no_telemetry_subscriber")
        for case_id in ("cli_discovery", "cli_task_start", "cli_state_read"):
            self.assertEqual(telemetrydiag.eligibility({"kind": "process", "case_id": case_id,
                                                        "instance_id": case_id})[0],
                             "records_no_telemetry")
        self.assertIsNone(telemetrydiag.eligibility({"kind": "process",
                                                     "case_id": "cli_task_run_check",
                                                     "instance_id": TASK_RUN}))


class TelemetryChildRuns(ChildRunBase):
    """A trace child run with telemetry selected, contained over the stand-in runtime: the
    eligible Task run is recorded once per side and read back in its own namespace, and every
    other selected case is an omission naming the supported matrix."""

    def setUp(self):
        ChildRunBase.setUp(self)
        self.parent = planning.plan(self.git, planning.Request(
            trigger="manual", tier="quick", scope="partial_scope", base=self.root,
            head=self.root, run_id="parent_run", seed=5, trusted_host_build=True,
            cases=["canonical_serialize_small", "cli_discovery", "cli_task_run_check"]))
        with open(self.path("parent.json"), "wb") as stream:
            stream.write(contracts.write_bytes(self.parent, "run"))
        # Only this disposable copy is made writable; the sealed fixture may be read-only.
        self.docs = writable_copy(DOCUMENTS, self.path("docs"))

    def place(self):
        root = self.path("scratch-fs")
        os.mkdir(root, 0o700)
        return {"scratch_root": os.path.realpath(root),
                "docker": fakes.docker(self.path("docker"))}

    def run_child(self, telemetry="spans", **af):
        config = dict({"telemetry_docs": self.docs}, **af)
        return self.profile("linux_gnu_trace", [SMALL, DISCOVERY, TASK_RUN], {}, {},
                            af_config=config, contained=self.place(), telemetry=telemetry)

    def test_an_eligible_case_is_recorded_per_side_and_the_rest_name_the_matrix(self):
        doc, output = self.run_child()
        telemetry = doc["telemetry"]
        self.assertEqual((telemetry["level"], telemetry["perturbing"],
                          telemetry["verdict_effect"]), ("spans", False, "none"))
        self.assertEqual([(item["instance_id"], item["side"]) for item in telemetry["captures"]],
                         [(TASK_RUN, "baseline"), (TASK_RUN, "candidate")])
        for item in telemetry["captures"]:
            self.assertIsNone(item["omission"], item["omission"])
            self.assertEqual(item["evidence"]["state"], "complete")
            self.assertEqual(item["source_sha"], self.parent["resolved"][item["side"] + "_sha"])
            self.assertEqual(sorted(a["path"].rsplit("/", 1)[1] for a in item["artifacts"]),
                             sorted(diagcontracts.TELEMETRY_DOCUMENTS))
            for artifact in item["artifacts"]:
                self.assertTrue(artifact["path"].startswith("telemetry/%s/%s/"
                                                            % (TASK_RUN, item["side"])))
        omitted = dict((item["instance_id"], item["reason"]) for item in doc["omissions"]
                       if item["kind"] == "telemetry")
        self.assertEqual(omitted, {SMALL: "no_telemetry_subscriber",
                                   DISCOVERY: "records_no_telemetry"})
        # The traces of the recipe are still there, beside the telemetry.
        self.assertTrue(doc["traces"])
        page = read(os.path.join(output, diagnostics.PAGE_FILE)).decode()
        self.assertIn("<h2>Telemetry</h2>", page)
        self.assertIn("never contributes a verdict sample", page)
        # The child bundle is admitted by the report, which derives every capture again.
        child = report.admit_child(output, self.parent)
        state = report._case_telemetry(TASK_RUN, [child])
        self.assertEqual(state["state"], "available")
        self.assertEqual(sorted(set(item["kind"] for item in state["items"])),
                         ["telemetry", "telemetry_perfetto", "timeline"])
        self.assertEqual(report._case_telemetry(DISCOVERY, [child])["state"], "omitted")

    def test_content_is_labelled_perturbing(self):
        doc, output = self.run_child(telemetry="content")
        self.assertEqual((doc["telemetry"]["level"], doc["telemetry"]["perturbing"]),
                         ("content", True))
        self.assertIn("content is perturbing", read(os.path.join(
            output, diagnostics.PAGE_FILE)).decode())

    def test_a_reader_that_fails_is_an_omission_whose_written_documents_stay_counted(self):
        doc, _ = self.run_child(telemetry_fail=["status"])
        for item in doc["telemetry"]["captures"]:
            self.assertEqual(item["omission"]["reason"], "telemetry_read_failed")
            self.assertIsNone(item["evidence"])
            names = sorted(a["path"].rsplit("/", 1)[1] for a in item["artifacts"])
            self.assertIn("runs.json", names)
            self.assertIn("show.json", names)

    def test_a_partial_recording_is_never_labelled_complete(self):
        docs = loaded()
        docs["status"]["segments"]["unsealed"] = 1
        for kind, data in dumped(docs).items():
            with open(os.path.join(self.docs, kind + ".json"), "wb") as stream:
                stream.write(data)
        doc, _ = self.run_child()
        for item in doc["telemetry"]["captures"]:
            self.assertEqual(item["evidence"]["state"], "partial")
            self.assertEqual(item["evidence"]["reasons"], ["history_partial", "unsealed_segment"])

    def test_a_tampered_child_is_refused_by_the_report_even_with_a_consistent_manifest(self):
        _, output = self.run_child()
        show = os.path.join(output, "telemetry", TASK_RUN, "baseline", "show.json")
        doc = json.loads(read(show).decode("utf-8"))
        doc["spans"][3]["status"] = "open"
        del doc["spans"][3]["total_us"], doc["spans"][3]["self_us"]
        with open(show, "wb") as stream:
            stream.write(json.dumps(doc).encode("utf-8"))
        # The tamperer rewrites the hashes too: only the derivation can tell.
        profiles = json.loads(read(os.path.join(output, "profiles.json")).decode("utf-8"))
        for item in profiles["telemetry"]["captures"]:
            for artifact in item["artifacts"]:
                if artifact["path"].endswith("/baseline/show.json"):
                    data = read(show)
                    artifact["bytes"] = len(data)
                    artifact["sha256"] = "sha256:" + __import__("hashlib").sha256(
                        data).hexdigest()
        with open(os.path.join(output, "profiles.json"), "wb") as stream:
            stream.write(contracts.write_bytes(profiles, "profiles"))
        os.unlink(os.path.join(output, bundle.MANIFEST))
        bundle.write_manifest(output, 1 << 30)
        with self.assertRaises(ContractError) as caught:
            report.admit_child(output, self.parent)
        self.assertIn("telemetry", str(caught.exception))

    def test_report_rederives_missing_duplicate_wrong_state_and_count_evidence(self):
        _, output = self.run_child()
        original = read(os.path.join(output, "telemetry", TASK_RUN, "baseline", "status.json"))
        profiles_path = os.path.join(output, "profiles.json")
        original_profiles = json.loads(read(profiles_path))
        for mutation in ("missing", "duplicate", "wrong_execution", "mismatched_state", "sealed_count"):
            with self.subTest(mutation=mutation):
                status = json.loads(original)
                entry = status["bytes"]["entries"][0]
                if mutation == "missing":
                    status["bytes"]["entries"].pop(0)
                elif mutation == "duplicate":
                    status["bytes"]["entries"].append(dict(entry))
                elif mutation == "wrong_execution":
                    entry["name"] = "another-execution"
                elif mutation == "mismatched_state":
                    entry["state"] = "live"
                    status["segments"].update(sealed=0, live=1)
                else:
                    status["segments"]["sealed"] = 0
                path = os.path.join(output, "telemetry", TASK_RUN, "baseline", "status.json")
                data = json.dumps(status).encode()
                with open(path, "wb") as stream:
                    stream.write(data)
                profiles = json.loads(json.dumps(original_profiles))
                for capture in profiles["telemetry"]["captures"]:
                    for artifact in capture["artifacts"]:
                        if artifact["path"].endswith("/baseline/status.json"):
                            artifact["bytes"] = len(data)
                            artifact["sha256"] = "sha256:" + __import__("hashlib").sha256(data).hexdigest()
                with open(profiles_path, "wb") as stream:
                    stream.write(contracts.write_bytes(profiles, "profiles"))
                os.unlink(os.path.join(output, bundle.MANIFEST))
                bundle.write_manifest(output, 1 << 30)
                with self.assertRaises(ContractError):
                    report.admit_child(output, self.parent)

    def test_report_rederives_execution_summaries_and_cross_reader_inventory_with_updated_hashes(self):
        _, output = self.run_child()
        profiles_path = os.path.join(output, "profiles.json")
        original_profiles = json.loads(read(profiles_path))
        traces = []
        for entry in original_profiles["traces"]:
            paths = [artifact["path"] for artifact in entry["artifacts"]
                     if artifact["path"].endswith("/trace.json")]
            traces.append(None if entry["omission"] is not None else
                          phasetrace.load(read(os.path.join(output, paths[0]))))
        for name, docs, _ in inventory_cases():
            with self.subTest(mutation=name):
                profiles = json.loads(json.dumps(original_profiles))
                data_by_kind = dumped(docs)
                for kind, data in data_by_kind.items():
                    path = os.path.join(output, "telemetry", TASK_RUN, "baseline", kind + ".json")
                    with open(path, "wb") as stream:
                        stream.write(data)
                for capture in profiles["telemetry"]["captures"]:
                    if capture["side"] != "baseline":
                        continue
                    for artifact in capture["artifacts"]:
                        kind = artifact["path"].rsplit("/", 1)[1].removesuffix(".json")
                        if kind in data_by_kind:
                            data = data_by_kind[kind]
                            artifact.update(bytes=len(data), sha256="sha256:" +
                                            __import__("hashlib").sha256(data).hexdigest())
                with open(profiles_path, "wb") as stream:
                    stream.write(contracts.write_bytes(profiles, "profiles"))
                # Keep the trusted page consistent too, so its hash/view checks cannot
                # conceal a missing telemetry reconciliation in report admission.
                with open(os.path.join(output, "diagnostic.html"), "wb") as stream:
                    stream.write(diagnostics.page(profiles, traces))
                os.unlink(os.path.join(output, bundle.MANIFEST))
                bundle.write_manifest(output, 1 << 30)
                with self.assertRaisesRegex(ContractError, "telemetry"):
                    report.admit_child(output, self.parent)

    def test_consistent_observational_inventory_difference_remains_a_valid_partial_report(self):
        _, docs, _ = next(case for case in inventory_cases() if case[0] == "runs:only_here")
        for kind, data in dumped(docs).items():
            with open(os.path.join(self.docs, kind + ".json"), "wb") as stream:
                stream.write(data)
        doc, output = self.run_child()
        for entry in doc["telemetry"]["captures"]:
            self.assertIsNone(entry["omission"])
            self.assertEqual(entry["evidence"]["state"], "partial")
            self.assertIn("history_partial", entry["evidence"]["reasons"])
        report.admit_child(output, self.parent)

    def test_report_rederives_reverse_identity_with_consistent_figures_and_updated_hashes(self):
        for name, valid, invalid in identity_cases():
            with self.subTest(mutation=name):
                case_root = self.path(name)
                os.mkdir(case_root)
                with mock.patch.object(self, "tmp", case_root):
                    for kind, data in dumped(valid).items():
                        with open(os.path.join(self.docs, kind + ".json"), "wb") as stream:
                            stream.write(data)
                    _, output = self.run_child()
                    report.admit_child(output, self.parent)  # distinct identities are partial
                    profiles_path = os.path.join(output, "profiles.json")
                    profiles = json.loads(read(profiles_path))
                    # Forge only the immutable inventory identity; all displayed figures,
                    # trace/Perfetto/page bytes and their hashes remain consistent.
                    data_by_kind = dumped(invalid)
                    for capture in profiles["telemetry"]["captures"]:
                        if capture["side"] != "baseline":
                            continue
                        for artifact in capture["artifacts"]:
                            kind = artifact["path"].rsplit("/", 1)[1].removesuffix(".json")
                            if kind in data_by_kind:
                                data = data_by_kind[kind]
                                with open(os.path.join(output, artifact["path"]), "wb") as stream:
                                    stream.write(data)
                                artifact.update(bytes=len(data), sha256="sha256:" +
                                                __import__("hashlib").sha256(data).hexdigest())
                    with open(profiles_path, "wb") as stream:
                        stream.write(contracts.write_bytes(profiles, "profiles"))
                    traces = []
                    for entry in profiles["traces"]:
                        paths = [a["path"] for a in entry["artifacts"]
                                 if a["path"].endswith("/trace.json")]
                        traces.append(None if entry["omission"] is not None else
                                      phasetrace.load(read(os.path.join(output, paths[0]))))
                    with open(os.path.join(output, "diagnostic.html"), "wb") as stream:
                        stream.write(diagnostics.page(profiles, traces))
                    os.unlink(os.path.join(output, bundle.MANIFEST))
                    bundle.write_manifest(output, 1 << 30)
                    with self.assertRaisesRegex(ContractError, "span identity"):
                        report.admit_child(output, self.parent)

    def test_telemetry_is_refused_beside_counters_and_without_a_level(self):
        doc, _ = self.run_child()
        broken = json.loads(json.dumps(doc))
        broken["telemetry"]["level"] = None
        broken["telemetry"]["perturbing"] = False
        with self.assertRaises(ContractError):
            diagcontracts.validate_profiles(broken)
        broken = json.loads(json.dumps(doc))
        broken["recipe"]["kind"] = "counters"
        with self.assertRaises(ContractError):
            diagcontracts.validate_profiles(broken)
        broken = json.loads(json.dumps(doc))
        broken["telemetry"]["captures"][0]["evidence"]["reasons"] = ["open_spans"]
        with self.assertRaises(ContractError):
            diagcontracts.validate_profiles(broken)
        broken = json.loads(json.dumps(doc))
        broken["telemetry"]["captures"][0]["evidence"]["open_spans"] = 2
        with self.assertRaises(ContractError):
            diagcontracts.validate_profiles(broken)


class ProbeTrace(TempDir):
    """`cli.py probe --trace-output`, the investigation's entry point: the measured request's
    phase trace beside its judged sample, never the seed writer's, never a judged input."""

    TRACE = {"schema": "af.perf-trace/1", "pid": 7, "clock": "monotonic_ns",
             "epoch_unix_ns": "1", "capacity": 64, "dropped": 0, "open": 0,
             "spans": [{"id": 1, "parent": None, "phase": "probe_region", "tid": 1,
                        "start_ns": 0, "end_ns": 1000000},
                       {"id": 2, "parent": 1, "phase": "task_runtime_new", "tid": 1,
                        "start_ns": 10, "end_ns": 999000},
                       {"id": 3, "parent": 2, "phase": "task_projection", "tid": 1,
                        "start_ns": 20, "end_ns": 400000},
                       {"id": 4, "parent": 2, "phase": "task_projection", "tid": 1,
                        "start_ns": 500000, "end_ns": 990000}]}

    def setUp(self):
        TempDir.setUp(self)
        self.doc, self.golden = oracle.load(SUITE)
        os.mkdir(self.path("scratch"), 0o700)
        os.mkdir(self.path("supervisor"))
        self.supervisor = fake_supervisor(self.path("supervisor", "perf-supervisor"))

    def sample(self, traces, **config):
        probe = fakes.probe(self.path("probe-bin"), self.golden, **config)
        result, _ = oracle.sample(self.doc, self.golden, self.supervisor, probe, SMALL,
                                  self.path("scratch"), 30, trace=traces)
        self.assertEqual(os.listdir(self.path("scratch")), [], "the namespace is removed")
        return result

    def test_the_trace_is_returned_beside_the_judged_sample_and_breaks_down_its_region(self):
        traces = []
        result = self.sample(traces, trace=self.TRACE)
        self.assertEqual(result["status"], "passed", result)
        self.assertEqual(json.loads(traces[0].decode()), self.TRACE)
        lines = phasetrace.region_breakdown(phasetrace.load(traces[0]))
        self.assertEqual(lines[0], "region probe_region #1: 1.000 ms")
        self.assertIn("task_runtime_new", lines[1])
        self.assertEqual(len([line for line in lines if "task_projection" in line]), 2)
        plain = self.sample(None, trace=self.TRACE)
        self.assertEqual(plain["status"], "passed")

    def test_a_probe_without_perf_trace_exports_nothing_and_a_link_is_never_read(self):
        traces = []
        self.assertEqual(self.sample(traces)["status"], "passed")
        self.assertEqual(traces, [None])
        os.makedirs(self.path("elsewhere"))
        with self.assertRaises(ContractError):
            self.sample([], trace_symlink=self.path("elsewhere", "secret"))
