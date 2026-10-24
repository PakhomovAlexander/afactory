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


class Documents(TempDir):
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
            self.assertEqual(item["evidence"]["reasons"], ["unsealed_segment"])

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
