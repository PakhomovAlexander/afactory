"""The trusted driver's oracle: requests, identity, side-specific failures and golden checks.

Messages here are constructed by the test from golden entries to exercise the judge; they are
not probe output and claim nothing about the product. Real probe executions are judged by the
Rust integration test `crates/review-pipeline/tests/perf_probe.rs`.
"""
import copy
import json
import os
import subprocess
import sys

from perflab import contracts, limits, oracle, reference, supervise
from perflab import suite as suite_module
from perflab.values import ContractError
from tests.support import CLI, ROOT, SUITE, TempDir, fake_supervisor

LOADED = []


def loaded():
    """The registry and golden document, loaded (and reference-checked) once per process."""
    if not LOADED:
        LOADED.append(oracle.load(SUITE))
    return LOADED[0]


def fake_probe(path, body):
    """A stand-in probe executable: a shell script that is never product code."""
    with open(path, "w") as stream:
        stream.write("#!/bin/sh\n" + body + "\n")
    os.chmod(path, 0o755)
    return path


def copied_suite(test, edit_golden=None):
    """The registry and its golden document copied below the test's directory."""
    with open(SUITE, "rb") as stream:
        text = stream.read()
    os.makedirs(test.path("golden"))
    suite_path = test.path("suite.toml")
    with open(suite_path, "wb") as stream:
        stream.write(text)
    with open(os.path.join(ROOT, "fixtures/performance/golden/native.json"), "rb") as stream:
        golden = json.loads(stream.read())
    if edit_golden is not None:
        edit_golden(golden)
    with open(test.path("golden", "native.json"), "wb") as stream:
        stream.write(contracts.write_bytes(golden, "golden"))
    return suite_path


def message_for(request, entry, **changes):
    doc = {"schema": "af.perf-probe/1", "case_id": request["case_id"],
           "instance_id": request["instance_id"], "contract_id": request["contract_id"],
           "fixture_digest": entry["fixture_digest"], "parameters": request["parameters"],
           "metrics": {"wall_ms": {"value": "12.5", "unit": "ms"},
                       "setup_ms": {"value": "3", "unit": "ms"}},
           "correctness": {"status": "passed", "digest": entry["digest"],
                           "count": entry["count"], "detail": "ok: test-constructed"}}
    for key, value in changes.items():
        doc[key] = value
    return contracts.canonical_bytes(doc) + b"\n"


SEED_COPY = "/perf/scratch/seed/probe-work"


def request_of(doc, instance_id, scratch):
    """The registry's request of an instance, naming a seed copy when it reads a seed."""
    seed = SEED_COPY if oracle.seed_of(doc, instance_id) is not None else None
    return oracle.request_for(doc, instance_id, scratch, seed)


class Judge(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.doc, self.golden = loaded()
        self.entries = dict((entry["instance_id"], entry) for entry in self.golden["entries"])
        self.request = oracle.request_for(
            self.doc, "canonical_serialize__package_issue_255_large", self.tmp)
        self.entry = self.entries[self.request["instance_id"]]

    def judge(self, stdout, returncode=0, request=None):
        return oracle.judge(self.doc, self.golden, request or self.request, returncode, stdout)

    def failure(self, result):
        self.assertEqual(result["status"], "failed")
        return result["failure"]["kind"]

    def test_requests_carry_the_registry_identity(self):
        self.assertEqual(self.request, {
            "schema": "af.perf-probe-request/2", "case_id": "canonical_serialize",
            "instance_id": "canonical_serialize__package_issue_255_large",
            "contract_id": "canonical_serialize_v1",
            "parameters": {"package": "issue_255_large"}, "repetitions": 1,
            "scratch": self.tmp, "seed": None})
        sentinel = oracle.request_for(self.doc, "sentinel_canonical", self.tmp)
        self.assertEqual((sentinel["contract_id"], sentinel["repetitions"], sentinel["parameters"]),
                         ("sentinel_canonical_v1", 64, {}))
        capture = oracle.request_for(
            self.doc, "source_capture__files_10000_tree_source_files_128b", self.tmp, SEED_COPY)
        self.assertEqual(capture["parameters"], {"files": 10000, "tree": "source_files_128b"})
        self.assertEqual((capture["contract_id"], capture["seed"]),
                         ("source_capture_v2", SEED_COPY))
        # A seed reader names its private copy, and nothing else names one (ADR-0170).
        with self.assertRaises(ContractError) as caught:
            oracle.request_for(self.doc, capture["instance_id"], self.tmp)
        self.assertIn("reads a seed", str(caught.exception))
        with self.assertRaises(ContractError) as caught:
            oracle.request_for(self.doc, self.request["instance_id"], self.tmp, SEED_COPY)
        self.assertIn("reads no seed", str(caught.exception))
        with self.assertRaises(ContractError) as caught:
            oracle.request_for(self.doc, capture["instance_id"], "/perf/scratch",
                               "/perf/scratch/seed")
        self.assertIn("outside the probe's scratch", str(caught.exception))
        for undeclared in ("cli_discovery__command_version", "canonical_serialize__package_x"):
            with self.assertRaises(ContractError):
                oracle.request_for(self.doc, undeclared, self.tmp)

    def test_request_validation_refuses_wrong_identity_and_fields(self):
        for change, message in (({"instance_id": "canonical_serialize__package_small"},
                                 "does not name the assignment"),
                                ({"scratch": "relative/dir"}, "does not match"),
                                ({"extra": 1}, "unknown field extra"),
                                ({"repetitions": 0}, "must be within"),
                                ({"parameters": {"package": 1.5}}, "expected")):
            with self.subTest(change=change):
                with self.assertRaises(ContractError) as caught:
                    contracts.validate_probe_request(dict(self.request, **change))
                self.assertIn(message, str(caught.exception))

    def test_a_message_matching_the_golden_entry_passes(self):
        result = self.judge(message_for(self.request, self.entry))
        self.assertEqual(result["status"], "passed", result)
        self.assertEqual(result["oracle"], {"status": "passed",
                                            "observed_digest": self.entry["digest"]})
        self.assertEqual(result["metrics"]["wall_ms"]["value"], "12.5")

    def test_golden_mismatches_fail_the_producing_side(self):
        wrong = "sha256:" + "1" * 64
        for changes, field in ((dict(correctness=dict(status="passed", digest=wrong,
                                                      count=self.entry["count"], detail="ok")),
                                "digest"),
                               (dict(correctness=dict(status="passed",
                                                      digest=self.entry["digest"],
                                                      count=5605229, detail="ok")), "count"),
                               (dict(fixture_digest=wrong), "fixture_digest")):
            with self.subTest(field=field):
                result = self.judge(message_for(self.request, self.entry, **changes))
                self.assertEqual(self.failure(result), "oracle_mismatch")
                self.assertIn(field, result["failure"]["detail"])
                self.assertEqual(result["oracle"]["status"], "failed")

    def test_a_self_reported_pass_is_not_sufficient(self):
        # The probe says passed, with a count it did not get from the golden entry.
        bogus = message_for(self.request, dict(self.entry, count=1, digest="sha256:" + "2" * 64))
        self.assertEqual(self.failure(self.judge(bogus)), "oracle_mismatch")

    def test_exact_parameter_and_contract_identity(self):
        for changes in (dict(parameters={"package": "issue_255_medium"}),
                        dict(instance_id="canonical_serialize__package_issue_255_medium"),
                        dict(contract_id="canonical_validate_v1"),
                        dict(case_id="canonical_validate")):
            with self.subTest(changes=changes):
                result = self.judge(message_for(self.request, self.entry, **changes))
                self.assertEqual(self.failure(result), "oracle_mismatch")
                self.assertIn("names a different", result["failure"]["detail"])
        capture = oracle.request_for(
            self.doc, "source_capture__files_100_tree_source_files_128b", self.tmp, SEED_COPY)
        entry = self.entries[capture["instance_id"]]
        spelled = message_for(capture, entry, parameters={"files": "100",
                                                          "tree": "source_files_128b"})
        self.assertEqual(self.failure(self.judge(spelled, request=capture)), "oracle_mismatch")
        self.assertEqual(self.judge(message_for(capture, entry), request=capture)["status"],
                         "passed")

    def test_functional_failures_keep_their_kind(self):
        expired = message_for(self.request, self.entry, correctness=dict(
            status="failed", digest=None, count=0,
            detail="lease_expired: region=current_plan_check; Task lease expired"))
        self.assertEqual(self.failure(self.judge(expired, 1)), "lease_expired")
        broken = message_for(self.request, self.entry, correctness=dict(
            status="failed", digest=None, count=0, detail="operation_failed: corrupt"))
        self.assertEqual(self.failure(self.judge(broken, 1)), "nonzero_exit")
        self.assertEqual(self.failure(self.judge(message_for(self.request, self.entry), 1)),
                         "nonzero_exit")
        self.assertEqual(self.failure(self.judge(b"", 2)), "launch_failure")
        self.assertEqual(self.failure(self.judge(b"", 101)), "nonzero_exit")

    def test_malformed_unbounded_or_extra_output_fails(self):
        line = message_for(self.request, self.entry)
        self.assertEqual(self.failure(self.judge(line + line)), "oracle_mismatch")
        self.assertEqual(self.failure(self.judge(line[:-1])), "oracle_mismatch")
        self.assertEqual(self.failure(self.judge(b"{}\n")), "oracle_mismatch")
        padded = line[:-2] + b" " * (64 * 1024) + b"}\n"
        self.assertEqual(self.failure(self.judge(padded)), "oracle_mismatch")

    def test_metrics_must_be_declared_and_complete(self):
        no_wall = message_for(self.request, self.entry,
                              metrics={"setup_ms": {"value": "3", "unit": "ms"}})
        self.assertEqual(self.failure(self.judge(no_wall)), "missing_required_metric")
        undeclared = message_for(self.request, self.entry, metrics={
            "wall_ms": {"value": "1", "unit": "ms"}, "setup_ms": {"value": "1", "unit": "ms"},
            "instructions": {"value": "9", "unit": "count"}})
        self.assertEqual(self.failure(self.judge(undeclared)), "oracle_mismatch")
        wrong_unit = message_for(self.request, self.entry, metrics={
            "wall_ms": {"value": "1", "unit": "bytes"}, "setup_ms": {"value": "1", "unit": "ms"}})
        self.assertEqual(self.failure(self.judge(wrong_unit)), "oracle_mismatch")

    def test_the_judge_command_reads_a_recorded_execution(self):
        request = self.path("request.json")
        stdout = self.path("stdout")
        with open(request, "wb") as stream:
            stream.write(contracts.canonical_bytes(self.request))
        with open(stdout, "wb") as stream:
            stream.write(message_for(self.request, self.entry))
        result = subprocess.run([sys.executable, "-I", CLI, "judge", "--request", request,
                                 "--stdout", stdout, "--exit-code", "0"], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["status"], "passed")
        with open(stdout, "wb") as stream:
            stream.write(message_for(self.request, dict(self.entry, count=0)))
        result = subprocess.run([sys.executable, "-I", CLI, "judge", "--request", request,
                                 "--stdout", stdout, "--exit-code", "0"], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(json.loads(result.stdout)["failure"]["kind"], "oracle_mismatch")

    def test_the_probe_command_judges_a_supervised_process_and_owns_only_its_scratch(self):
        # A stand-in executable that refuses every request, as a probe does for an
        # undeclared case: the driver must report launch_failure and remove its scratch.
        fake = self.path("refusing-probe")
        with open(fake, "w") as stream:
            stream.write("#!/bin/sh\ncat >/dev/null\necho refused >&2\nexit 2\n")
        os.chmod(fake, 0o755)
        supervisor = fake_supervisor(self.path("supervisor"))
        root = self.path("scratch")
        os.mkdir(root, 0o700)
        argv = [sys.executable, "-I", CLI, "probe", "--supervisor", supervisor, "--binary",
                fake, "--instance", "sentinel_canonical", "--scratch", root]
        refused = subprocess.run(argv, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(refused.returncode, 2, refused.stderr)
        self.assertIn(b"--trusted-local", refused.stderr)
        result = subprocess.run(argv + ["--trusted-local"], cwd=ROOT, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 3, result.stderr)
        reported = json.loads(result.stdout)
        self.assertEqual(reported["failure"]["kind"], "launch_failure")
        self.assertEqual(reported["capability"]["scope"], "diagnostic_single_sample")
        self.assertEqual(reported["capability"]["containment"], "trusted_local_process_group")
        self.assertEqual(reported["capability"]["same_user_isolation"], "not_provided")
        self.assertEqual(reported["supervision"]["lifecycle"], "no_observed_descendants")
        self.assertEqual(os.listdir(root), [])

    def test_a_spawning_case_is_refused_before_anything_is_created(self):
        marker = self.path("launched")
        probe = fake_probe(self.path("probe"), "touch '%s'" % marker)
        root = self.path("scratch")
        os.mkdir(root, 0o700)
        instance = "source_capture__files_100_tree_source_files_128b"
        self.assertEqual(oracle.capability(self.doc, instance)["linux_containment"],
                         "required_not_provided")
        with self.assertRaises(oracle.Unsupported):
            oracle.sample(self.doc, self.golden, fake_supervisor(self.path("supervisor")),
                          probe, instance, root, 10)
        result = subprocess.run([sys.executable, "-I", CLI, "probe", "--trusted-local",
                                 "--supervisor", self.path("supervisor"), "--binary", probe,
                                 "--instance", instance, "--scratch", root], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn(b"requires Linux namespace containment", result.stderr)
        self.assertFalse(os.path.exists(marker))
        self.assertEqual(os.listdir(root), [])


class Coverage(TempDir):
    def test_golden_must_match_the_registry_identity_and_constants(self):
        doc, golden = loaded()
        changed = copy.deepcopy(golden)
        changed["entries"][0]["contract_id"] = "another_v1"
        with self.assertRaises(ContractError) as caught:
            oracle.check_coverage(doc, changed)
        self.assertIn("identity differs", str(caught.exception))
        changed = copy.deepcopy(golden)
        changed["entries"].append(dict(changed["entries"][-1], instance_id="zz_extra",
                                       case_id="zz_extra", parameters={}))
        with self.assertRaises(ContractError) as caught:
            oracle.check_coverage(doc, changed)
        self.assertIn("golden entries for undeclared zz_extra", str(caught.exception))
        # Every seed a native instance reads has exactly its writer's golden entry.
        changed = copy.deepcopy(golden)
        changed["seeds"] = changed["seeds"][1:]
        with self.assertRaises(ContractError) as caught:
            oracle.check_coverage(doc, changed)
        self.assertIn("golden.seeds: no golden entry for", str(caught.exception))
        changed = copy.deepcopy(golden)
        changed["seeds"][0]["contract_id"] = "another_v1"
        with self.assertRaises(ContractError) as caught:
            oracle.check_coverage(doc, changed)
        self.assertIn("identity differs", str(caught.exception))

    def test_list_refuses_a_registry_whose_golden_entries_are_missing(self):
        def drop_first(golden):
            golden["entries"] = golden["entries"][1:]
        suite_path = copied_suite(self, drop_first)
        result = subprocess.run([sys.executable, "-I", CLI, "list", "--suite", suite_path],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 2)
        self.assertIn(b"no golden entry for", result.stderr)

    def test_probe_commands_refuse_a_golden_document_the_reference_does_not_derive(self):
        # Coverage and schema still hold; only the sentinel's digest was changed. An output
        # agreeing with the changed digest must not pass: the document is refused first.
        altered = "sha256:" + "1" * 64

        def alter(golden):
            for entry in golden["entries"]:
                if entry["instance_id"] == "sentinel_canonical":
                    entry["digest"] = altered
        suite_path = copied_suite(self, alter)
        doc = suite_module.load(suite_path).doc
        with open(self.path("golden", "native.json"), "rb") as stream:
            golden = contracts.read_json(stream.read(), "golden")
        oracle.check_coverage(doc, golden)
        with self.assertRaises(ContractError) as caught:
            oracle.load(suite_path)
        self.assertIn("is not what perflab.reference derives", str(caught.exception))
        request = oracle.request_for(doc, "sentinel_canonical", self.tmp)
        entry = dict(next(item for item in golden["entries"]
                          if item["instance_id"] == "sentinel_canonical"))
        request_path = self.path("request.json")
        stdout_path = self.path("stdout")
        with open(request_path, "wb") as stream:
            stream.write(contracts.canonical_bytes(request))
        with open(stdout_path, "wb") as stream:
            stream.write(message_for(request, entry))
        result = subprocess.run([sys.executable, "-I", CLI, "judge", "--suite", suite_path,
                                 "--request", request_path, "--stdout", stdout_path,
                                 "--exit-code", "0"],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn(b"is not what perflab.reference derives", result.stderr)
        marker = self.path("launched")
        probe = fake_probe(self.path("probe"), "touch '%s'\nexit 2" % marker)
        os.mkdir(self.path("scratch"))
        result = subprocess.run([sys.executable, "-I", CLI, "probe", "--suite", suite_path,
                                 "--binary", probe, "--instance", "sentinel_canonical",
                                 "--scratch", self.path("scratch")],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(os.path.exists(marker), "nothing launches on a refused golden")

    def test_the_committed_golden_document_loads(self):
        doc, golden = loaded()
        self.assertEqual(len(golden["entries"]), 23)
        self.assertEqual([entry["instance_id"] for entry in golden["seeds"]], [
            "authority_seed__package_issue_255_large",
            "authority_seed__package_issue_255_medium",
            "source_capture_seed__files_10000_tree_source_files_128b",
            "source_capture_seed__files_100_tree_source_files_128b",
            "state_projection_seed__log_events_10000"])
        self.assertEqual(golden, reference.golden_document(doc))
        # A writer's check covers exactly what its readers' golden entries hold.
        entries = dict((entry["instance_id"], entry) for entry in golden["entries"])
        seeds = dict((entry["instance_id"], entry) for entry in golden["seeds"])
        for reader, seed in (("authority_plan_check__package_issue_255_large",
                              "authority_seed__package_issue_255_large"),
                             ("authority_runtime_new__package_issue_255_medium",
                              "authority_seed__package_issue_255_medium"),
                             ("state_projection__log_events_10000_projection_prewarmed",
                              "state_projection_seed__log_events_10000"),
                             ("source_capture__files_100_tree_source_files_128b",
                              "source_capture_seed__files_100_tree_source_files_128b")):
            self.assertEqual((entries[reader]["digest"], entries[reader]["count"]),
                             (seeds[seed]["digest"], seeds[seed]["count"]))


class RequestIdentity(TempDir):
    """A recorded request must be exactly the registry's declaration of its instance."""

    def setUp(self):
        TempDir.setUp(self)
        self.doc, self.golden = loaded()
        self.entries = dict((entry["instance_id"], entry) for entry in self.golden["entries"])

    def recorded(self, declared, **changes):
        request = dict(request_of(self.doc, declared, self.tmp), **changes)
        return request, message_for(request, self.entries[declared])

    def test_every_declared_request_is_admitted_and_its_golden_output_passes(self):
        for instance_id in sorted(self.entries):
            with self.subTest(instance=instance_id):
                request, line = self.recorded(instance_id)
                self.assertIs(oracle.check_request(self.doc, request), request)
                extra = {}
                if request["contract_id"] == "persistence_fs_v1":
                    expected = reference.persistence_logical_bytes(
                        request["parameters"]["operation"])
                    extra = {"logical_bytes": {"value": str(expected or 4096),
                                               "unit": "bytes"}}
                metrics = dict({"wall_ms": {"value": "1", "unit": "ms"},
                                "setup_ms": {"value": "1", "unit": "ms"}}, **extra)
                line = message_for(request, self.entries[instance_id], metrics=metrics)
                result = oracle.judge(self.doc, self.golden, request, 0, line)
                self.assertEqual(result["status"], "passed", result)

    def test_a_sentinel_request_with_fewer_repetitions_is_refused(self):
        # The output copies the golden digest and count of the declared 64 repetitions.
        request, line = self.recorded("sentinel_canonical", repetitions=1)
        with self.assertRaises(ContractError) as caught:
            oracle.judge(self.doc, self.golden, request, 0, line)
        self.assertIn("differs from the registry's declaration of sentinel_canonical in "
                      "repetitions", str(caught.exception))
        request, line = self.recorded("canonical_serialize__package_issue_255_large",
                                      repetitions=2)
        with self.assertRaises(ContractError):
            oracle.judge(self.doc, self.golden, request, 0, line)

    def test_every_request_identity_field_must_equal_the_registry(self):
        large = "canonical_serialize__package_issue_255_large"
        capture = "source_capture__files_100_tree_source_files_128b"
        refused = (
            # Another case is another instance; its contract still names this one's.
            (large, dict(case_id="canonical_validate",
                         instance_id="canonical_validate__package_issue_255_large"),
             "in contract_id"),
            (large, dict(contract_id="canonical_validate_v1"), "in contract_id"),
            (large, dict(case_id="canonical_serialize_small",
                         instance_id="canonical_serialize_small__package_issue_255_large"),
             "has no available native oracle"),
            (capture, dict(parameters={"files": 100, "tree": "source_files_128b",
                                       "extra": "x"},
                           instance_id="source_capture__extra_x_files_100_tree_source_files_128b"),
             "has no available native oracle"),
            ("sentinel_canonical", dict(repetitions=65), "in repetitions"),
            (large, dict(instance_id="canonical_serialize__package_issue_255_medium"),
             "does not name the assignment"),
        )
        for instance_id, changes, reason in refused:
            with self.subTest(changes=changes):
                request, line = self.recorded(instance_id, **changes)
                with self.assertRaises(ContractError) as caught:
                    oracle.judge(self.doc, self.golden, request, 0, line)
                self.assertIn(reason, str(caught.exception))

    def test_a_parameter_of_the_wrong_type_is_refused(self):
        capture = "source_capture__files_100_tree_source_files_128b"
        request = dict(request_of(self.doc, capture, self.tmp),
                       parameters={"files": "100", "tree": "source_files_128b"})
        with self.assertRaises(ContractError) as caught:
            oracle.check_request(self.doc, request)
        self.assertIn("parameters.files", str(caught.exception))

    def test_the_judge_command_refuses_an_undeclared_repetition_count(self):
        request, line = self.recorded("sentinel_canonical", repetitions=1)
        request_path = self.path("request.json")
        stdout_path = self.path("stdout")
        with open(request_path, "wb") as stream:
            stream.write(contracts.canonical_bytes(request))
        with open(stdout_path, "wb") as stream:
            stream.write(line)
        result = subprocess.run([sys.executable, "-I", CLI, "judge", "--request", request_path,
                                 "--stdout", stdout_path, "--exit-code", "0"], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, b"")
        self.assertIn(b"repetitions", result.stderr)


class SeedWriters(TempDir):
    """A seed writer's request and its check against the golden seed entry (ADR-0170)."""

    def setUp(self):
        TempDir.setUp(self)
        self.doc, self.golden = loaded()
        self.seeds = dict((entry["instance_id"], entry) for entry in self.golden["seeds"])

    def test_a_writer_request_is_the_registry_s_and_its_check_is_judged(self):
        seed_id = "state_projection_seed__log_events_10000"
        self.assertEqual(oracle.seed_of(
            self.doc, "state_projection__log_events_10000_projection_prewarmed")["seed_id"],
            seed_id)
        request = oracle.seed_request(self.doc, seed_id, self.tmp)
        self.assertEqual((request["case_id"], request["contract_id"], request["parameters"],
                          request["seed"]),
                         ("state_projection_seed", "state_projection_seed_v1",
                          {"log": "events_10000"}, None))
        self.assertIs(oracle.check_request(self.doc, request), request)
        entry = self.seeds[seed_id]
        metrics = {"wall_ms": {"value": "1", "unit": "ms"},
                   "setup_ms": {"value": "1", "unit": "ms"}}
        result = oracle.judge(self.doc, self.golden, request, 0,
                              message_for(request, entry, metrics=metrics))
        self.assertEqual(result["status"], "passed", result)
        for changes in (dict(correctness=dict(status="passed", digest=entry["digest"],
                                              count=9999, detail="ok")),
                        dict(correctness=dict(status="passed", digest="sha256:" + "3" * 64,
                                              count=10000, detail="ok"))):
            result = oracle.judge(self.doc, self.golden, request, 0,
                                  message_for(request, entry, metrics=metrics, **changes))
            self.assertEqual(result["failure"]["kind"], "oracle_mismatch")
        # A writer reads no seed, and a seed no instance reads has no writer.
        with self.assertRaises(ContractError):
            oracle.check_request(self.doc, dict(request, seed=SEED_COPY))
        with self.assertRaises(ContractError):
            oracle.seed_request(self.doc, "state_projection_seed__log_events_1", self.tmp)


class LogicalBytes(TempDir):
    """Both persistence variants report logical bytes; the CAS variant's are exact."""

    def setUp(self):
        TempDir.setUp(self)
        self.doc, self.golden = loaded()
        self.entries = dict((entry["instance_id"], entry) for entry in self.golden["entries"])

    def judge(self, operation, logical):
        instance_id = "persistence_fs__operation_" + operation
        request = oracle.request_for(self.doc, instance_id, self.tmp)
        metrics = {"wall_ms": {"value": "12.5", "unit": "ms"},
                   "setup_ms": {"value": "3", "unit": "ms"}}
        if logical is not None:
            metrics["logical_bytes"] = logical
        line = message_for(request, self.entries[instance_id], metrics=metrics)
        return oracle.judge(self.doc, self.golden, request, 0, line)

    def test_the_cas_variant_needs_exactly_the_reference_logical_bytes(self):
        exact = str(2 * reference.PERSISTENCE_BLOB_BYTES)
        self.assertEqual(self.entries["persistence_fs__operation_cas_write_read"]["count"],
                         int(exact))
        passed = self.judge("cas_write_read", {"value": exact, "unit": "bytes"})
        self.assertEqual(passed["status"], "passed", passed)
        missing = self.judge("cas_write_read", None)
        self.assertEqual(missing["failure"]["kind"], "missing_required_metric")
        self.assertIn("logical_bytes", missing["failure"]["detail"])
        for value, reason in (("1", "differs from the reference"),
                              (str(int(exact) + 1), "differs from the reference"),
                              ("0", "canonical positive integer"),
                              ("02097152", "logical_bytes"),
                              ("2097152.5", "canonical positive integer")):
            with self.subTest(value=value):
                result = self.judge("cas_write_read", {"value": value, "unit": "bytes"})
                self.assertEqual(result["failure"]["kind"], "oracle_mismatch", result)
                self.assertIn(reason, result["failure"]["detail"])
        for unit in ("ms", "count"):
            with self.subTest(unit=unit):
                result = self.judge("cas_write_read", {"value": exact, "unit": unit})
                self.assertEqual(result["failure"]["kind"], "oracle_mismatch")
                self.assertIn("logical_bytes", result["failure"]["detail"])

    def test_the_state_variant_needs_positive_logical_bytes_without_a_fixed_golden(self):
        # Its bytes include variable event metadata, so no golden value is claimed for them.
        self.assertIsNone(reference.persistence_logical_bytes("state_append_read"))
        for value in ("1", "48213"):
            passed = self.judge("state_append_read", {"value": value, "unit": "bytes"})
            self.assertEqual(passed["status"], "passed", passed)
        missing = self.judge("state_append_read", None)
        self.assertEqual(missing["failure"]["kind"], "missing_required_metric")
        for logical in ({"value": "0", "unit": "bytes"}, {"value": "007", "unit": "bytes"},
                        {"value": "1.5", "unit": "bytes"}, {"value": "100", "unit": "count"}):
            with self.subTest(logical=logical):
                result = self.judge("state_append_read", logical)
                self.assertEqual(result["failure"]["kind"], "oracle_mismatch", result)


class SupervisedExecution(TempDir):
    """Every diagnostic sample runs under the supervisor; its outcome is judged before output."""

    def setUp(self):
        TempDir.setUp(self)
        self.doc, self.golden = loaded()
        self.supervisor = fake_supervisor(self.path("supervisor"))
        os.mkdir(self.path("scratch"), 0o700)
        entry = next(item for item in self.golden["entries"]
                     if item["instance_id"] == "sentinel_canonical")
        self.line = self.path("line")
        request = oracle.request_for(self.doc, "sentinel_canonical", self.tmp)
        with open(self.line, "wb") as stream:
            stream.write(message_for(request, entry))

    def sample(self, body, timeout=10):
        probe = fake_probe(self.path("probe"), body)
        result, stderr = oracle.sample(self.doc, self.golden, self.supervisor, probe,
                                       "sentinel_canonical", self.path("scratch"), timeout)
        self.assertEqual(os.listdir(self.path("scratch")), [])
        return result, stderr

    def test_a_valid_message_passes_and_reports_its_supervision(self):
        result, _ = self.sample("cat >/dev/null\ncat '%s'" % self.line)
        self.assertEqual(result["status"], "passed", result)
        self.assertIsNotNone(result["supervision"]["child_wall_ns"])
        failed, _ = self.sample("cat >/dev/null\ncat '%s'\nexit 1" % self.line)
        self.assertEqual(failed["failure"]["kind"], "nonzero_exit")

    def test_an_unconfirmed_supervised_end_keeps_the_scratch_for_recovery(self):
        # ADR-0171: nothing is removed beneath a process that may still be live. The first
        # supervisor reports an uncertain cleanup, the second no valid reply at all.
        with open(self.path("overrides.json"), "w") as stream:
            json.dump({"0": "uncertain", "1": "garbage"}, stream)
        probe = fake_probe(self.path("probe"), "cat >/dev/null\ncat '%s'" % self.line)
        result, stderr = oracle.sample(self.doc, self.golden, self.supervisor, probe,
                                       "sentinel_canonical", self.path("scratch"), 10)
        self.assertEqual(result["failure"]["kind"], "lifecycle_violation")
        kept = os.listdir(self.path("scratch"))
        self.assertEqual(len(kept), 1)
        self.assertIn(("the private scratch %s was kept" % os.path.join(
            os.path.realpath(self.path("scratch")), kept[0])).encode(), stderr)
        with self.assertRaises(ContractError) as caught:
            oracle.sample(self.doc, self.golden, self.supervisor, probe, "sentinel_canonical",
                          self.path("scratch"), 10)
        self.assertIn("no reply confirms that its sample ended", str(caught.exception))
        self.assertIn("was kept", str(caught.exception))
        self.assertEqual(len(os.listdir(self.path("scratch"))), 2)

    def test_stdout_beyond_the_control_record_is_an_oracle_mismatch(self):
        result, _ = self.sample("cat >/dev/null\nhead -c 70000 /dev/zero")
        self.assertEqual(result["failure"]["kind"], "oracle_mismatch")
        self.assertIn("control record", result["failure"]["detail"])
        self.assertEqual(result["supervision"]["stdout_truncated"],
                         70000 - limits.RECORD_BYTES_MAX)

    def test_stderr_beyond_the_log_cap_is_counted_not_judged(self):
        log_bytes = self.doc["limits"]["sample_log_bytes"]
        body = "cat >/dev/null\nhead -c %d /dev/zero >&2\ncat '%s'" % (log_bytes, self.line)
        result, stderr = self.sample(body)
        self.assertEqual(result["status"], "passed", result)
        self.assertGreater(result["supervision"]["stderr_truncated"], 0)
        self.assertLessEqual(len(stderr), log_bytes)

    def test_a_probe_that_never_finishes_times_out(self):
        result, _ = self.sample("cat >/dev/null\nexec sleep 60", timeout=1)
        self.assertEqual(result["failure"]["kind"], "timeout")

    def test_lifecycle_outcomes_precede_the_oracle(self):
        request = oracle.request_for(self.doc, "sentinel_canonical", self.tmp)
        entry = next(item for item in self.golden["entries"]
                     if item["instance_id"] == "sentinel_canonical")
        passing = message_for(request, entry)
        cases = (("unexpected_descendants", "lifecycle_violation", False),
                 ("cleanup_uncertain", "lifecycle_violation", True))
        for status, kind, stop in cases:
            with self.subTest(status=status):
                supervised = supervised_reply(passing, lifecycle=status)
                self.assertEqual(supervise.outcome(supervised, "pid_namespace")[0::2],
                                 (kind, stop))
                result = oracle.judge_supervised(self.doc, self.golden, request, supervised,
                                                 "pid_namespace")
                self.assertEqual(result["failure"]["kind"], kind)
        # Pipes alone are accepted only from the explicitly trusted local containment.
        quiet = supervised_reply(passing, lifecycle="no_observed_descendants")
        self.assertEqual(oracle.judge_supervised(self.doc, self.golden, request, quiet,
                                                 "process_group")["status"], "passed")
        self.assertEqual(supervise.outcome(quiet, "pid_namespace")[0], "lifecycle_violation")


def supervised_reply(stdout, lifecycle="clean", termination=None):
    observation = "pipes_only" if lifecycle == "no_observed_descendants" \
        else "pid_namespace_scan"
    # The counts each status rests on: descendants at the exit, then what remained.
    counts = {"clean": (0, 0), "unexpected_descendants": (1, 0), "cleanup_uncertain": (0, 1),
              "no_observed_descendants": (None, None)}[lifecycle]
    result = {
        "schema": contracts.SUPERVISOR_RESULT_SCHEMA,
        "supervisor": {"platform": "linux", "arch": "x86_64", "namespace_init": True,
                       "containment": "process_group" if observation == "pipes_only"
                       else "pid_namespace"},
        "termination": termination or {"kind": "exited", "code": 0, "signal": None,
                                       "detail": ""},
        "timing": {"child_wall_ns": "1000", "exit_observation": "unreaped",
                   "before_launch_ns": "1", "cleanup_ns": "1", "after_exit_ns": "1"},
        "stdout": {"retained": len(stdout), "total": len(stdout), "truncated": 0},
        "stderr": {"retained": 0, "total": 0, "truncated": 0},
        "stdout_pattern_counts": [], "stdout_json": None, "setup": None,
        "lifecycle": {"observation": observation, "descendants_at_exit": counts[0],
                      "remaining_after_cleanup": counts[1], "pipes_held": False,
                      "status": lifecycle, "detail": "constructed by the test"},
        "scratch": None, "rusage": None, "rusage_unavailable": "constructed by the test",
        "observations": {"before": {}, "after": {}, "unavailable": "constructed by the test"}}
    contracts.validate_supervisor_result(result)
    return supervise.Supervised(result, stdout, b"", 1, b"")


class TimeoutInput(TempDir):
    def test_an_invalid_explicit_timeout_launches_nothing(self):
        marker = self.path("launched")
        probe = fake_probe(self.path("probe"), "touch '%s'\ncat >/dev/null\nexit 2" % marker)
        supervisor = fake_supervisor(self.path("supervisor"))
        scratch = self.path("scratch")
        os.mkdir(scratch, 0o700)
        argv = [sys.executable, "-I", CLI, "probe", "--trusted-local", "--supervisor",
                supervisor, "--binary", probe, "--instance", "sentinel_canonical",
                "--scratch", scratch, "--timeout"]
        for value in ("0", "-1", "121", "1.5", "x"):
            with self.subTest(timeout=value):
                result = subprocess.run(argv + [value], cwd=ROOT, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertFalse(os.path.exists(marker))
                self.assertEqual(os.listdir(scratch), [])
        for value in (True, 0, 121, 2.0, None):
            with self.assertRaises(ContractError):
                oracle.check_timeout(value)
        self.assertEqual(oracle.check_timeout(120), 120)
        # Within bounds, an explicit timeout is used; omitted, the case's declared one is.
        result = subprocess.run(argv + ["120"], cwd=ROOT, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertTrue(os.path.exists(marker))
        doc, _ = loaded()
        self.assertEqual(oracle.timeout_for(
            doc, "state_projection__log_events_10000_projection_application_cold"), 120)
        self.assertEqual(oracle.timeout_for(doc, "sentinel_canonical"), 30)


def read(path):
    with open(path, "rb") as stream:
        return stream.read()


class PrivateNamespace(TempDir):
    """Trusted local scratch: removed through handles and verified tombstones; a replacement
    is never removed and never reported as success (ADR-0166)."""

    def setUp(self):
        TempDir.setUp(self)
        self.parent = os.path.realpath(self.path("parent"))
        os.mkdir(self.parent, 0o700)
        # A sibling in the caller's parent and a directory outside it: never touched.
        with open(os.path.join(self.parent, "sibling"), "wb") as stream:
            stream.write(b"the caller's")
        self.outside = self.path("outside")
        os.mkdir(self.outside)
        with open(os.path.join(self.outside, "kept"), "wb") as stream:
            stream.write(b"outside")
        self.addCleanup(self.untouched)

    def untouched(self):
        self.assertEqual(read(os.path.join(self.parent, "sibling")), b"the caller's")
        self.assertEqual(os.listdir(self.outside), ["kept"])

    def foreign(self, path):
        """Swap the entry at `path` for a foreign directory, keeping the original aside."""
        os.rename(path, path + ".moved")
        os.mkdir(path)
        with open(os.path.join(path, "theirs"), "wb") as stream:
            stream.write(b"foreign")

    def foreign_empty(self, path):
        """Another invocation's empty private directory, indistinguishable by mode and owner."""
        os.rename(path, path + ".moved")
        os.mkdir(path, 0o700)

    def link(self, path):
        os.rename(path, path + ".moved")
        os.symlink(self.outside, path)

    def at(self, stage, replace):
        """A boundary hook that runs `replace` at one stage and records the paths it saw."""
        seen = {}

        def boundary(name, path):
            seen.setdefault("path", path)
            seen.setdefault(name, path)
            if name == stage and "replaced" not in seen:
                seen["replaced"] = path
                replace(path)
        return seen, boundary

    def assert_left(self, path):
        """The entry a replacement put at `path` is exactly as the replacement left it."""
        self.assertTrue(os.path.isdir(path + ".moved"))
        if os.path.islink(path):
            self.assertEqual(os.readlink(path), self.outside)
        else:
            self.assertEqual(read(os.path.join(path, "theirs")), b"foreign")

    def tombstones(self, directory):
        return [name for name in os.listdir(directory)
                if name.startswith(oracle.TOMBSTONE_PREFIX)]

    def test_a_held_namespace_is_removed_without_following_symlinks(self):
        namespace = oracle.Namespace(self.parent)
        self.assertEqual(os.stat(namespace.path).st_mode & 0o7777, 0o700)
        work = os.path.join(namespace.path, "probe-work")
        os.makedirs(os.path.join(work, "nested"))
        with open(os.path.join(work, "nested", "file"), "wb") as stream:
            stream.write(b"probe-owned")
        os.symlink(self.outside, os.path.join(work, "link"))
        os.symlink(os.path.join(self.outside, "kept"), os.path.join(namespace.path, "file-link"))
        os.mkfifo(os.path.join(work, "fifo"))
        self.assertEqual(namespace.remove(), [])
        self.assertEqual(os.listdir(self.parent), ["sibling"])

    def test_entries_created_after_a_first_enumeration_are_removed(self):
        # The Linux regression: a held descriptor enumerated once while empty did not see
        # entries created later. Creation enumerates the empty namespace first, then the
        # probe-like writes happen, then removal must see every one of them.
        namespace = oracle.Namespace(self.parent)
        self.assertEqual(oracle._listing(namespace._fd), [])
        os.makedirs(os.path.join(namespace.path, "probe-work", "value"))
        with open(os.path.join(namespace.path, "probe-work", "value", "data"), "wb") as stream:
            stream.write(b"x")
        self.assertEqual(oracle._listing(namespace._fd), ["probe-work"])
        os.mkdir(os.path.join(namespace.path, "later"))
        self.assertEqual(oracle._listing(namespace._fd), ["later", "probe-work"])
        self.assertEqual(namespace.remove(), [])
        self.assertEqual(os.listdir(self.parent), ["sibling"])

    def test_a_replacement_before_the_handle_is_refused_when_it_is_distinguishable(self):
        # Between `mkdir` and the handle: a foreign non-empty directory, a symlink, or an
        # empty directory of another mode is refused, left in place, and nothing is removed.
        def empty_0755(path):
            self.foreign_empty(path)
            os.chmod(path, 0o755)
        for replace in (self.foreign, self.link, empty_0755):
            with self.subTest(replace=replace.__name__):
                seen, boundary = self.at("created", replace)
                with self.assertRaises(ContractError) as caught:
                    oracle.Namespace(self.parent, boundary)
                self.assertIn("was left in place and nothing was launched",
                              str(caught.exception))
                path = seen["path"]
                self.assertTrue(os.path.isdir(path + ".moved"))
                if replace is not empty_0755:
                    self.assert_left(path)
                else:
                    self.assertEqual(os.listdir(path), [])

    def test_an_indistinguishable_replacement_is_outside_the_declared_boundary(self):
        # The trusted local boundary is the private parent: another user cannot reach it, and
        # this same user is trusted. An empty 0700 directory of this user swapped in before
        # the handle exists has nothing that tells it apart, so the driver does not claim
        # exclusive ownership against it; every sample says so in its capability.
        doc, _ = loaded()
        self.assertEqual(oracle.capability(doc, "sentinel_canonical")["same_user_isolation"],
                         "not_provided")
        self.addCleanup(os.chmod, self.parent, 0o700)
        for mode in (0o750, 0o770, 0o1777, 0o757):
            with self.subTest(mode=oct(mode)):
                os.chmod(self.parent, mode)
                if mode & 0o022:
                    with self.assertRaises(ContractError) as caught:
                        oracle.Namespace(self.parent)
                    self.assertIn("writable by no one else", str(caught.exception))
                else:
                    self.assertEqual(oracle.Namespace(self.parent).remove(), [])
                self.assertEqual(os.listdir(self.parent), ["sibling"])

    def test_a_child_swapped_after_it_was_cleared_is_never_removed(self):
        # Between a child's clearing and its removal another empty directory takes its name:
        # the tombstone shows it is not the held child, it is put back, and cleanup fails.
        for replace in (self.foreign_empty, self.foreign, self.link):
            with self.subTest(replace=replace.__name__):
                seen, boundary = self.at("child_cleared", replace)
                namespace = oracle.Namespace(self.parent, boundary)
                os.makedirs(os.path.join(namespace.path, "probe-work", "value"))
                problems = namespace.remove()
                self.assertTrue(problems)
                self.assertIn("was replaced", problems[0])
                child = seen["replaced"]
                self.assertTrue(os.path.lexists(child), "the replacement was removed")
                self.assertTrue(os.path.isdir(child + ".moved"))
                if replace.__name__ != "foreign_empty":
                    self.assert_left(child)
                self.assertEqual(self.tombstones(os.path.dirname(child)), [])
                self.assertTrue(os.path.isdir(namespace.path), "a failed clear removed the "
                                "namespace")

    def test_a_replacement_before_the_namespace_removal_is_left_in_place(self):
        for replace in (self.foreign_empty, self.foreign, self.link):
            with self.subTest(replace=replace.__name__):
                seen, boundary = self.at("cleared", replace)
                namespace = oracle.Namespace(self.parent, boundary)
                os.makedirs(os.path.join(namespace.path, "probe-work", "nested"))
                problems = namespace.remove()
                self.assertEqual(len(problems), 1, problems)
                self.assertIn("was replaced; the entry now there was left in place",
                              problems[0])
                path = seen["path"]
                self.assertTrue(os.path.lexists(path))
                if replace.__name__ != "foreign_empty":
                    self.assert_left(path)
                # The namespace's own contents went through its handle, wherever it is now.
                self.assertEqual(os.listdir(path + ".moved"), [])
                self.assertEqual(self.tombstones(self.parent), [])

    def test_the_residual_posix_window_is_detected_and_never_success(self):
        # POSIX has no removal by handle. A same-user process that found the random tombstone
        # name between its verification and the `rmdir` could have its empty directory taken;
        # the held namespace is then seen to survive, so the cleanup is uncertain and fails.
        seen, boundary = self.at("tombstoned", self.foreign_empty)
        namespace = oracle.Namespace(self.parent, boundary)
        problems = namespace.remove()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("cleanup is uncertain", problems[0])
        self.assertTrue(os.path.isdir(seen["replaced"] + ".moved"))

    def test_a_vanished_namespace_is_reported(self):
        _, boundary = self.at("cleared", os.rmdir)
        problems = oracle.Namespace(self.parent, boundary).remove()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("vanished", problems[0])

    def test_an_unconfirmed_cleanup_fails_the_sample_and_keeps_its_original_failure(self):
        doc, golden = loaded()
        supervisor = fake_supervisor(self.path("supervisor"))
        request = oracle.request_for(doc, "sentinel_canonical", self.tmp)
        entry = next(item for item in golden["entries"]
                     if item["instance_id"] == "sentinel_canonical")
        line = self.path("line")
        with open(line, "wb") as stream:
            stream.write(message_for(request, entry))
        passing = "cat >/dev/null\ncat '%s'\nexit 0" % line
        for body, kind in ((passing, "lifecycle_violation"),
                           ("cat >/dev/null\nexit 2", "launch_failure")):
            with self.subTest(kind=kind):
                probe = fake_probe(self.path("probe"), body)
                seen, boundary = self.at("cleared", self.foreign)
                result, stderr = oracle.sample(doc, golden, supervisor, probe,
                                               "sentinel_canonical", self.parent, 10, boundary)
                self.assertEqual(result["status"], "failed")
                self.assertEqual(result["failure"]["kind"], kind)
                self.assertIn("was not removed", result["failure"]["detail"])
                self.assertIn(b"perf-probe driver: the private scratch", stderr)
                self.assert_left(seen["path"])
                if kind == "launch_failure":
                    # The original failure leads; the cleanup is appended, never instead.
                    self.assertTrue(result["failure"]["detail"].startswith(
                        "the probe refused its request; "), result)
        # A confirmed cleanup passes and leaves nothing of the sample in the parent.
        before = sorted(os.listdir(self.parent))
        result, _ = oracle.sample(doc, golden, supervisor, fake_probe(self.path("probe"),
                                                                      passing),
                                  "sentinel_canonical", self.parent, 10)
        self.assertEqual(result["status"], "passed", result)
        self.assertEqual(sorted(os.listdir(self.parent)), before)

    def test_a_long_cleanup_note_never_erases_the_original_diagnostic(self):
        doc, _ = loaded()
        request = oracle.request_for(doc, "sentinel_canonical", self.tmp)
        original = "original operation diagnostic: Task lease expired while recording"
        problems = ["could not remove %s/entry-%04d: Permission denied" % (self.tmp, index)
                    for index in range(80)]
        for status in ("failed", "passed"):
            with self.subTest(status=status):
                result = (oracle._failed(request, "lease_expired", original)
                          if status == "failed" else
                          oracle._result(request, "passed", None, {}, "passed", None))
                result, note = oracle._cleanup_failed(request, result, self.tmp, problems)
                detail = result["failure"]["detail"]
                self.assertGreater(len(note), 4096)
                self.assertLessEqual(len(detail), 4096)
                self.assertTrue(detail.startswith(original if status == "failed"
                                                  else "the sample passed; "), detail[:200])
                self.assertIn("the private scratch %s was not removed" % self.tmp, detail)
                self.assertIn("more characters]", detail)
                self.assertEqual(result["failure"]["kind"],
                                 "lease_expired" if status == "failed" else
                                 "lifecycle_violation")
        long_original = "o" * 6000
        result = oracle._failed(request, "nonzero_exit", long_original)
        result, _ = oracle._cleanup_failed(request, result, self.tmp, problems)
        detail = result["failure"]["detail"]
        self.assertLessEqual(len(detail), 4096)
        self.assertTrue(detail.startswith("o" * 2000))
        self.assertIn("was not removed", detail)
