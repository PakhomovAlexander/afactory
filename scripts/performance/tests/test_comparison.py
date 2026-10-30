"""One durable native comparison: declaration, partitioned core, aggregate, lanes and export.

The real journal, Git retention, claims, offline verification, aggregation, rendering and
bundle verification; builds, shards and diagnostic children are injected stand-ins (the
executor tests' synthetic computation), never Docker, perf or a speed claim.
"""
import copy
import io
import os
import shutil
import signal
from types import SimpleNamespace
from unittest import mock

from perflab import comparison, contracts, diagnostics, executor, hostobserver, overview, synthetic
from perflab import telemetrylab
from perflab.values import ContractError, sha256_bytes
from tests import telemetry_fixtures as fx
from tests.support import ROOT, read
from tests.test_executor import ExecutorFixture, IMAGE


class Comparison(ExecutorFixture):
    def setUp(self):
        super().setUp()
        control = self.repo.commit("diagnostic authority", {diagnostics.REGISTRY_PATH:
            read(os.path.join(ROOT, diagnostics.REGISTRY_PATH))})
        self.base = self.head = control
        self.c["control_sha"] = control
        tools = self.path("tools")
        os.mkdir(tools)
        for name in ("perf", "inferno-collapse-perf", "inferno-diff-folded", "inferno-flamegraph"):
            with open(os.path.join(tools, name), "w") as f:
                f.write("#!/bin/sh\nexit 0\n")
            os.chmod(os.path.join(tools, name), 0o755)
        # The whole family in two shards: quick's 25-minute window after a 20-minute build
        # fits one shard per bounded 60-minute original, so the comparison has two partitions.
        self.c["core"].update(cases=[], shards=2)
        self.c["diagnostic"].update(perf=os.path.join(tools, "perf"), inferno_bin=tools,
                                    observer="host_perf")
        self.probes = []

        def probe(start, perf, event, frequency, workdir, deadline, clock=None, proc=None):
            # The observer boundary itself is exercised in test_hostobserver; here it must run
            # under the machine exclusion, before any build.
            self.probes.append(os.path.isdir(os.path.join(self.locks,
                                                          "af-perf-machine.lock.fence")))
            return True, None, "3 task-clock:u samples of an owned gated process"
        patcher = mock.patch.object(hostobserver, "probe", probe)
        patcher.start()
        self.addCleanup(patcher.stop)
        executor.Journal.lock_directory = self.locks
        self.addCleanup(setattr, executor.Journal, "lock_directory", None)

    def launch(self, j, key="rel_1", head=None):
        return comparison.preflight(j, self.base, head or self.head, key, launch=True)

    def run_partition(self, j, staged, calls):
        return self.run_partition_with(staged, calls, lambda: j.cycle(lock_directory=self.locks))

    def run_partition_with(self, staged, calls, operation):
        class Builder:
            def __init__(self, owner, plan, toolchain, **kw):
                self.layout = SimpleNamespace(build=os.path.join(
                    owner, "target/performance", plan["run_id"], "build"))
                self.plan = plan

            def build(self):
                calls.append("build")
                os.makedirs(self.layout.build)
                with open(os.path.join(self.layout.build, "af"), "w") as f:
                    f.write("synthetic build")
                return synthetic.builds(self.plan)

        def prepare(image, mount, size_bytes, **kwargs):
            with open(image, "xb") as f:
                f.write(b"x")
            os.mkdir(mount)

        def shard(options, machine_lease=None):
            machine_lease.check()
            calls.append("shard_%d" % options.shard)
            target = os.path.join(options.run_dir, "shards", str(options.shard))
            shutil.copytree(os.path.join(staged, "shards", str(options.shard)), target)
            return contracts.read_file(os.path.join(target, "shard.json"), "shard")

        def cleanup(image, mount, **kwargs):
            os.rmdir(mount)
            os.unlink(image)
            return []
        with mock.patch.object(executor.builds, "Builder", Builder), \
                mock.patch.object(executor.hostprep, "prepare", prepare), \
                mock.patch.object(executor.hostprep, "cleanup", cleanup), \
                mock.patch.object(executor.runner, "run_shard", shard), \
                mock.patch.dict(os.environ, {"HOME": self.path("home"), "TMPDIR": self.tmp}):
            return operation()

    def staged(self, j, ident):
        doc, plan, _ = comparison.load(j, ident)
        return synthetic.write_run(plan, self.path("synthetic"),
                                   lambda slot, case: 200 if slot["side"] == "candidate" else 100)

    def stand_in(self, measurement):
        def execute(config, request, path, lease, installed):
            return {"status": "complete", "measurement_status": measurement, "evidence": [],
                    "actions": None, "detail": "stand-in"}
        return execute

    def telemetry_stand_in(self):
        """The telemetry original as a real native result bundle (`telemetrylab.run` over the
        fixture executor): its page links its manifest, raw documents and logs."""
        self.binaries = fx.binaries(self)

        def execute(config, request, path, lease, installed):
            telemetrylab.run(fx.settings(self, fx.StandIn(self.binaries),
                                         os.path.join(path, "run"),
                                         result_id="local_" + request["request_id"][4:]))
            return {"status": "complete", "measurement_status": "pass",
                    "evidence": executor.file_manifest(path, executor.EVIDENCE_PREFIXES),
                    "actions": None, "detail": "stand-in"}
        return execute

    def verify(self, j, ident, installed=executor.ROOT):
        """Core originals are verified natively; the telemetry and profile stand-ins have no
        native evidence, so their admission is injected."""
        if j.request(ident)["work"] == "core":
            return executor.verify_original(j, ident, installed)
        state = j.state(ident)
        return j.request(ident), state, {"measurement_status": "available"}

    def test_schedule_partitions_the_whole_family_into_bounded_serial_originals(self):
        plan = {"limits": {"build_minutes": 20, "shard_sampling_minutes": 45,
                           "run_total_minutes": 120},
                "shards": [{"index": i} for i in range(8)]}
        bounds, chunks = comparison.schedule(plan, 28800)
        self.assertEqual(chunks, [[0, 1], [2, 3], [4, 5], [6, 7]])
        self.assertEqual((bounds["children"], bounds["first_child_shards"],
                          bounds["later_child_shards"]), (4, 2, 2))
        for index, shards in enumerate(chunks):
            needed = executor.partition_seconds(plan, {"shards": shards, "build_from":
                                                       None if index == 0 else "exp_first"})
            self.assertLessEqual(needed, bounds["available_seconds"])
        with self.assertRaisesRegex(ContractError, "no window, sentinel or deadline is relaxed"):
            comparison.schedule(dict(plan, limits=dict(plan["limits"],
                                                        shard_sampling_minutes=110)), 28800)

    def test_preflight_refusals_are_retained_and_reserve_nothing(self):
        self.c["core"]["cases"] = ["canonical_serialize_small"]
        with executor.Journal(self.c) as j:
            refused = comparison.preflight(j, self.base, self.head, "partial", launch=True)
            self.assertEqual(refused["status"], "refused")
            self.assertIn("whole core family", refused["detail"])
            self.assertEqual(j.names(), [])
        self.c["core"]["cases"] = []
        self.c["diagnostic"]["perf"] = None
        with executor.Journal(self.c) as j:
            refused = comparison.preflight(j, self.base, self.head, "no_perf")
            self.assertEqual(refused["status"], "refused")
            self.assertIn("cpu_profile: profile refused before any build", refused["detail"])
            checks = dict((c["name"], c["state"]) for c in refused["checks"])
            self.assertEqual((checks["core_0"], checks["core_1"], checks["telemetry"],
                              checks["trace"], checks["cpu_profile"]),
                             ("passed", "passed", "passed", "passed", "refused"))
            self.assertEqual(j.names(), [])
        retained = sorted(os.listdir(os.path.join(self.c["root"], comparison.REFUSALS)))
        self.assertEqual(len(retained), 2)
        kept = contracts.read_json(read(os.path.join(self.c["root"], comparison.REFUSALS,
                                                     retained[-1])), "comparison-preflight")
        self.assertEqual(kept["status"], "refused")
        self.assertEqual(self.probes, [], "no capability probe for a refused configuration")
        # A configured image absent on this host refuses the whole comparison before any
        # member is submitted (`executor.image_admission`, shared with every original).
        self.c["diagnostic"]["perf"] = os.path.join(self.path("tools"), "perf")
        absent = ([{"name": "image_admission", "state": "unavailable", "detail": "absent"}],
                  "build_image_unavailable: the build image %s is not present" % IMAGE)
        with mock.patch.object(executor.Journal, "images", lambda *a: absent), \
                executor.Journal(self.c) as j:
            refused = comparison.preflight(j, self.base, self.head, "no_image", launch=True)
            self.assertEqual(refused["status"], "refused")
            self.assertIn("the build image %s is not present" % IMAGE, refused["detail"])
            self.assertEqual(j.names(), [])
        self.assertEqual(len(os.listdir(os.path.join(self.c["root"], comparison.REFUSALS))), 3)

    def test_full_comparison_partitions_aggregates_diagnoses_reports_and_exports(self):
        with executor.Journal(self.c) as j:
            ready = comparison.preflight(j, self.base, self.head, "rel_1")
            self.assertEqual(ready["status"], "ready", ready["detail"])
            self.assertEqual(self.probes, [True], "the observer probe ran under the fence")
            self.assertEqual(j.names(), [], "a preflight reserves nothing")
            doc = ready["comparison"]
            self.assertEqual([p["shards"] for p in doc["partitions"]], [[0], [1]])
            self.assertEqual([(d["lane"], d["recipe"], d["observer"]) for d in doc["diagnostics"]],
                             [("trace", "linux_gnu_trace", None),
                              ("cpu_profile", "linux_gnu_cpu_profile", "host_perf")])
            launched = self.launch(j)
            self.assertEqual(launched["status"], "launched", launched["detail"])
            ident = doc["comparison_id"]
            members = [p["request_id"] for p in doc["partitions"]] + [doc["telemetry"]["request_id"]]
            self.assertEqual(sorted(j.names()), sorted(members))
            # A resumed launch returns the same originals; another source pair is refused.
            self.assertEqual(self.launch(j)["status"], "launched")
            self.assertEqual(sorted(j.names()), sorted(members))
            other = self.launch(j, head=self.repo.commit("other", {"README": "other"}))
            self.assertEqual(other["status"], "refused")
            self.assertIn("another source pair", other["detail"])
            status = comparison.status(j, ident)
            self.assertEqual(status["status"], "running")
            self.assertEqual(status["lanes"][0]["state"], "queued")
            staged = self.staged(j, ident)
            calls = []
            self.assertEqual(self.run_partition(j, staged, calls), "complete")
            # Read-only status while another controller holds the journal's lock.
            with executor.Journal(self.c, readonly=True) as reader:
                self.assertEqual(comparison.status(reader, ident)["lanes"][0]["state"], "running")
            self.assertEqual(self.run_partition(j, staged, calls), "complete")
            # The second partition reused the first one's verified build: one build in all.
            self.assertEqual(calls, ["build", "shard_0", "shard_1"])
            second = j.request(doc["partitions"][1]["request_id"])
            executor.verify_original(j, second["request_id"])
            self.assertEqual(j.cycle(lock_directory=self.locks,
                                     execute=self.telemetry_stand_in()), "complete")
            advanced = comparison.advance(j, ident)
            agg, _ = comparison.read_aggregate(j, contracts.read_json(read(os.path.join(
                comparison.directory(j, ident), "comparison.json")), "comparison"))
            # The whole family's denominator over both partitions' shards, judged once. The
            # synthetic stand-in writes no seed copies, so the aggregate is honestly
            # `incomplete` (its seeded cases' records do not count), never a pass.
            plan = comparison.load(j, ident)[1]
            planned = sum(len(executor.runner.schedule.expected_slots(plan, s["index"]))
                          for s in plan["shards"])
            self.assertEqual(agg["coverage"]["required"], agg["coverage"]["observed"]
                             + agg["coverage"]["missing"])
            self.assertLessEqual(agg["coverage"]["required"], planned)
            self.assertEqual(agg["result"], "incomplete")
            self.assertTrue(agg["regressions"])
            self.assertTrue(all(p["included"] for p in agg["partitions"]))
            # The later partition ran in its own sealed window on the first one's build; the
            # family's run carries that authority, and the first partition carries none.
            first, later = (os.path.join(j.path(p["request_id"]), "run", "partitions.json")
                            for p in doc["partitions"])
            self.assertFalse(os.path.lexists(first))
            window = contracts.read_file(later, "partition-execution")
            self.assertEqual([(p["index"], p["request_id"], p["shards"])
                              for p in window["partitions"]],
                             [(1, doc["partitions"][1]["request_id"], [1])])
            self.assertEqual(read(os.path.join(comparison.directory(j, ident), "aggregate",
                                               "run", "partitions.json")), read(later))
            # The aggregate's regressions select the two diagnostic lanes' cases.
            lanes = dict((lane["lane"], lane) for lane in advanced["lanes"])
            self.assertEqual((lanes["trace"]["state"], lanes["cpu_profile"]["state"]),
                             ("queued", "queued"))
            self.assertTrue(advanced["diagnostic_selection"])
            profile = j.request(doc["diagnostics"][1]["request_id"])
            self.assertEqual(profile["parent"]["request_id"], ident)
            self.assertEqual(profile["plan_sha256"], doc["plan_sha256"])
            # Advancing again submits nothing new.
            names = sorted(j.names())
            comparison.advance(j, ident)
            self.assertEqual(sorted(j.names()), names)
            seen = []

            def profile_run(git, options, machine_lease=None):
                seen.append((options.recipe, options.observer))
                os.makedirs(options.output)
                with open(os.path.join(options.output, diagnostics.PROFILES_FILE), "w") as f:
                    f.write("{}")
                return 0, None, "stand-in profile"
            builder = SimpleNamespace(build=lambda: {"cleanup": {"status": "removed"}})
            with mock.patch.object(executor.builds, "Builder", lambda owner, plan, tc, **kw:
                                   SimpleNamespace(layout=SimpleNamespace(build=owner),
                                                   build=builder.build)), \
                    mock.patch.object(executor.hostprep, "prepare",
                                      lambda image, mount, size, **kw: os.mkdir(mount)), \
                    mock.patch.object(executor.hostprep, "cleanup",
                                      lambda image, mount, **kw: os.rmdir(mount) or []), \
                    mock.patch.object(diagnostics, "run_profile", profile_run):
                self.assertEqual(j.cycle(lock_directory=self.locks), "complete")
                self.assertEqual(j.cycle(lock_directory=self.locks), "complete")
            self.assertEqual(seen, [("linux_gnu_trace", None),
                                    ("linux_gnu_cpu_profile", "host_perf")])
            # Preflight, launch, resumed launch, then once more before the CPU profile's build;
            # every time under the machine fence.
            self.assertEqual(self.probes, [True] * 4)
            self.assertEqual(j.cycle(lock_directory=self.locks), "idle")
        with executor.Journal(self.c, readonly=True) as j, \
                mock.patch.object(overview, "_telemetry", lambda path: (
                    {"required": 3, "observed": 3, "failed": 0, "missing": 0},
                    {"verdict": "pass", "regressions": []}, "complete")), \
                mock.patch.object(comparison, "_profile_usable", lambda path, lane: ([], None)):
            unverified = comparison.status(j, ident)
            self.assertEqual(unverified["status"], "incomplete")
            self.assertIn("status does not verify evidence", " ".join(unverified["reasons"]))
            report = comparison.status(j, ident, verify=self.verify)
            self.assertEqual((report["status"], report["verification"]),
                             ("incomplete", "verified"))
            self.assertEqual(report["lanes"][0]["state"], "failed")
            self.assertIn("core aggregate result is incomplete", " ".join(report["reasons"]))
            # The same durable state with the family judged whole (injected): complete.
            judged = dict(agg, result="fail", coverage=dict(agg["coverage"], observed=agg[
                "coverage"]["required"], missing=0))
            with mock.patch.object(comparison, "read_aggregate", lambda journal, doc: (
                    judged, contracts.write_bytes(judged, "comparison-aggregate"))):
                complete = comparison.status(j, ident, verify=self.verify)
            self.assertEqual((complete["status"], complete["verification"]),
                             ("complete", "verified"), complete["reasons"])
            # Complete, and a real regression: completeness and the verdict are separate.
            self.assertEqual(complete["performance"]["core_verdict"], "fail")
            self.assertFalse(complete["calibration"]["eligible"])
            bundle = self.path("bundle")
            manifest, exported = comparison.export(j, ident, bundle, verify=self.verify)
        self.assertEqual(exported, report)
        moved = self.path("copied", "bundle")
        shutil.copytree(bundle, moved)
        shutil.rmtree(bundle)
        # The profile stand-in wrote no native children; their admission is recorded here
        # (exercised for real in test_report_diagnostics).
        admitted = []
        stand_in = mock.patch.object(comparison.report, "admit_children",
                                     lambda sources, plan: admitted.append(sources))
        stand_in.start()
        self.addCleanup(stand_in.stop)
        checked, checked_report = comparison.verify_export(moved)
        self.assertEqual(sorted(os.path.relpath(p[0], moved) for p in admitted),
                         sorted("evidence/requests/%s/profile" % lane["request_id"]
                                for lane in doc["diagnostics"]))
        # The operator command verifies the copied bundle with no journal or configuration.
        import cli
        with mock.patch("sys.stdout", new=io.StringIO()) as printed:
            self.assertEqual(cli.main(["executor", "comparison-verify", "--bundle", moved]), 0)
        self.assertIn('"status": "verified"', printed.getvalue())
        listed = [f["path"] for f in checked["files"]]
        # The nested native pages keep their links inside the copied bundle: the telemetry
        # result's page links its raw documents and logs; each core report its
        # evidence; the family aggregate its run.
        telemetry = "evidence/requests/%s/run/" % doc["telemetry"]["request_id"]
        page = telemetry + "report.html"
        nested = [resolved for _, resolved in comparison._links(
            read(os.path.join(moved, page)).decode(), page)]
        for part in ("raw/", "logs/"):
            self.assertTrue(any(path.startswith(telemetry + part) for path in nested), part)
        # Its native manifest, which its offline verification reads, travels with it.
        self.assertIn(telemetry + "manifest.json", listed)
        for path in nested:
            self.assertTrue(path in listed or any(f.startswith(path + "/") for f in listed), path)
        for item in doc["partitions"]:
            core = "evidence/requests/%s/" % item["request_id"]
            for name in ("run/run.json", "run/builds.json", "report/verdict.json",
                         "report/report.html"):
                self.assertIn(core + name, listed)
            self.assertTrue(any(f.startswith(core + "report/evidence/") for f in listed))
        self.assertIn("evidence/comparisons/%s/aggregate/run/run.json" % ident, listed)
        self.assertEqual(checked_report, report)
        self.assertIn("evidence/comparisons/%s/aggregate/report/verdict.json" % ident,
                      [f["path"] for f in checked["files"]])
        html_page = read(os.path.join(moved, "report", "report.html")).decode()
        self.assertIn('href="../evidence/comparisons/%s/aggregate/report/report.html"' % ident,
                      html_page)
        for link in report["links"]:
            self.assertTrue(os.path.isfile(os.path.join(moved, "evidence", link)), link)
        # A changed or an extra file is refused after copying.
        with open(os.path.join(moved, "evidence", report["links"][0]), "ab") as f:
            f.write(b" ")
        with self.assertRaisesRegex(ContractError, "changed after export"):
            comparison.verify_export(moved)
        with open(os.path.join(moved, "evidence", report["links"][0]), "rb+") as f:
            f.truncate(os.path.getsize(f.name) - 1)
        comparison.verify_export(moved)
        with open(os.path.join(moved, "evidence", "extra.txt"), "w") as f:
            f.write("not exported")
        with self.assertRaisesRegex(ContractError, "differ from its manifest"):
            comparison.verify_export(moved)
        os.unlink(os.path.join(moved, "evidence", "extra.txt"))
        comparison.verify_export(moved)
        raw = sorted(path for path in nested if path.startswith(telemetry + "raw/")
                     and path in listed)[0]

        forged_copies = []

        def forge(change):
            """A forger's copy: `change(copy)` alters it, and its export manifest is rehashed so
            every listed hash agrees with the bytes."""
            forged = self.path("forged-%d" % len(forged_copies))
            forged_copies.append(forged)
            shutil.copytree(moved, forged)
            change(forged)
            manifest = contracts.read_json(read(os.path.join(forged, "manifest.json")),
                                           "comparison-export")
            files = []
            for item in manifest["files"]:
                target = os.path.join(forged, item["path"])
                if os.path.lexists(target):
                    data = read(target)
                    files.append(dict(item, size=len(data), sha256=sha256_bytes(data)))
            manifest["files"] = files
            with open(os.path.join(forged, "manifest.json"), "wb") as f:
                f.write(contracts.write_bytes(manifest, "comparison-export"))
            return forged
        # Nested native evidence the telemetry page links, missing from a self-consistent copy.
        missing = forge(lambda root: os.unlink(os.path.join(root, raw)))
        with self.assertRaisesRegex(ContractError, "which the bundle does not hold"):
            comparison.verify_export(missing)

        def tamper(root):
            with open(os.path.join(root, raw), "ab") as f:
                f.write(b" ")
        # Nested native evidence changed and rehashed: the sealed result refuses it.
        with self.assertRaisesRegex(ContractError, "not evidence its sealed result names"):
            comparison.verify_export(forge(tamper))
        # A core partition's run bundle removed whole: its sealed run is not bundled.
        first = "evidence/requests/%s/run" % doc["partitions"][0]["request_id"]
        with self.assertRaisesRegex(ContractError, "sealed run is not bundled"):
            comparison.verify_export(forge(lambda root: shutil.rmtree(os.path.join(root,
                                                                                   first))))

    def test_a_controller_cancelled_in_a_shard_runs_no_later_shard_or_original(self):
        """SIGTERM to `comparison-run` during a shard: the runner's handler consumed it and
        sealed the shard (`runner.Cancelled`, what `run_shard` raises under the controller's
        lease). The partition's later shard, the next partition and the queued telemetry
        original never start; the cancelled partition ends interrupted with its partial
        evidence and a settled fence, and is never replayed."""
        # The nightly tier's 120-minute original holds two 45-minute shards after its build.
        self.c["core"].update(tier="nightly", environment="linux_x86_64_musl_claw", shards=3)
        self.c["zigbuild_bin"] = "/opt/zig/bin"
        with executor.Journal(self.c) as j:
            launched = self.launch(j)
            self.assertEqual(launched["status"], "launched", launched["detail"])
            doc = launched["comparison"]
            ident = doc["comparison_id"]
            partitions = [p["request_id"] for p in doc["partitions"]]
            self.assertEqual([p["shards"] for p in doc["partitions"]][0], [0, 1])
            staged = self.staged(j, ident)
        calls = []

        def cancel_in_first_shard(original):
            def shard(options, machine_lease=None):
                doc = original(options, machine_lease)
                if options.shard == 0:
                    raise executor.runner.Cancelled(signal.SIGTERM, doc)
                return doc
            return shard

        def cycle(journal):
            real = executor.runner.run_shard
            with mock.patch.object(executor.runner, "run_shard", cancel_in_first_shard(real)):
                return journal.cycle(lock_directory=self.locks)

        def journal(readonly):
            return executor.Journal(self.c, readonly=readonly)

        with mock.patch.object(executor.compare, "evaluate", side_effect=AssertionError("verdict")):
            with self.assertRaises(executor.runner.Cancelled):
                self.run_partition_with(staged, calls, lambda: comparison.run(journal, ident, cycle))
        self.assertEqual(calls, ["build", "shard_0"], "no later shard of the partition")
        with executor.Journal(self.c, readonly=True) as j:
            states = [j.state(i)["state"] for i in partitions + [doc["telemetry"]["request_id"]]]
            self.assertEqual(states, ["interrupted"] + ["queued"] * (len(states) - 1),
                             "no later partition or queued telemetry original was claimed")
            result = contracts.read_file(os.path.join(j.path(partitions[0]), "result.json"),
                                         "executor-result")
            self.assertIn("shards 1 and every later original were not started", result["detail"])
            self.assertIn("run/shards/0/shard.json", [e["path"] for e in result["evidence"]])
        # The fence settled on confirmed cleanup; the interrupted original never replays.
        executor.hostlock.MachineLock(self.locks).release()
        with executor.Journal(self.c) as j:
            claimed = []
            self.assertEqual(j.cycle(lock_directory=self.locks, execute=lambda c, request, *a: (
                claimed.append(request["request_id"]) or self.stand_in("pass")(c, request, *a))),
                "complete")
            self.assertEqual(claimed, [partitions[1]])
            self.assertEqual(j.state(partitions[0])["state"], "interrupted")

    def test_failed_partition_and_lost_controller_leave_the_family_incomplete(self):
        """A claimed partition whose controller was lost becomes uncertain and is never
        replayed; the next partition cannot reuse a build that was never admitted, so it fails
        before any launch; the aggregate counts every planned record missing and the lanes
        that need it say why. Nothing is complete."""
        with executor.Journal(self.c) as j:
            launched = self.launch(j)
            doc = launched["comparison"]
            ident = doc["comparison_id"]
            first = j.request(doc["partitions"][0]["request_id"])
            j.transition(first, "claimed")
        with executor.Journal(self.c) as j:
            calls = []
            staged = self.staged(j, ident)
            self.assertEqual(self.run_partition(j, staged, calls), "failed")
            self.assertEqual(calls, [], "no rebuild, no shard: the reuse is refused first")
            self.assertEqual(j.state(doc["partitions"][0]["request_id"])["state"], "uncertain")
            second = j.state(doc["partitions"][1]["request_id"])
            self.assertIn("first partition's build is not admitted", second["detail"])
            self.assertEqual(j.cycle(lock_directory=self.locks, execute=self.stand_in("pass")),
                             "complete")
            advanced = comparison.advance(j, ident)
            agg, _ = comparison.read_aggregate(j, doc)
            self.assertIsNone(agg["result"])
            self.assertEqual([(p["state"], p["included"]) for p in agg["partitions"]],
                             [("uncertain", False), ("failed", False)])
            self.assertEqual(sorted(j.names()), sorted(
                [p["request_id"] for p in doc["partitions"]] + [doc["telemetry"]["request_id"]]),
                "no profile is submitted without a verified aggregate")
        self.assertEqual(advanced["status"], "incomplete")
        lanes = dict((lane["lane"], lane["state"]) for lane in advanced["lanes"])
        self.assertEqual((lanes["core"], lanes["trace"], lanes["cpu_profile"]),
                         ("failed", "failed", "failed"))
        reasons = " ".join(advanced["reasons"])
        self.assertIn("core partition 0", reasons)
        self.assertIn("are missing records, not passed", reasons)
        # The contract itself refuses relabelling this as complete.
        with self.assertRaisesRegex(ContractError, "complete only when"):
            contracts.write_bytes(dict(advanced, status="complete"), "comparison-report")
        bad = copy.deepcopy(agg)
        bad["result"] = "pass"
        bad["verdict_sha256"] = bad["builds_sha256"] = "sha256:" + "0" * 64
        bad["coverage"] = {"required": 1, "observed": 1, "failed": 0, "missing": 0}
        with self.assertRaisesRegex(ContractError, "missing partition never passes"):
            contracts.write_bytes(bad, "comparison-aggregate")
