"""Independent executor: real journal/Git/locks/admission, injected bounded compute.

Synthetic samples and tool stand-ins exercise orchestration only, never Docker or speed.
"""
import copy
import io
import json
import os
import runpy
import shutil
import signal
import subprocess
import sys
import time
from types import SimpleNamespace
from unittest import mock

from perflab import (archive, budget, builds, bundle, calibration, contracts, diagnostics, executor, gitrefs,
                     hostlock, localcalibration, planning, runner, synthetic, telemetrylab,
                     tomlfile, telemetryexec)
from perflab.values import ContractError, canonical_digest, document_bytes, sha256_bytes
from tests import build_fixtures
from tests import telemetry_fixtures as tf
from tests.support import ROOT, TempDir, lab_repo, read

LAB = os.path.join(ROOT, "scripts/performance")
IMAGE = "registry.invalid/sample@sha256:" + "1" * 64
# The actual local image ID a full comparison's build admitted (`docker image inspect` ID).
LOCAL_IMAGE_ID = "sha256:4f5e1e215d31e1e7844332afa5c4585bc63cdad88ae6e0b9237ffdbdeefbc2e1"
PRODUCER = synthetic.label_digest("installed-producer-test-only")
# The journal root's filesystem as the admission boundary sees it: tests never depend on the
# host's actual free space (`Journal.free_bytes`).
FILESYSTEM_FREE = 1 << 50


REAL_IMAGE_ADMISSION = executor.image_admission
REAL_JOURNAL_IMAGES = executor.Journal.images


def plenty_of_space(test):
    """The admission boundaries a synthetic journal cannot reach on its host: the root's
    filesystem space and the configured images, which `ImageAdmission` exercises through a
    stand-in docker client."""
    for patcher in (mock.patch.object(executor.Journal, "free_bytes",
                                      staticmethod(lambda path: FILESYSTEM_FREE)),
                    mock.patch.object(executor.Journal, "images",
                                      lambda self, config, recipes: ([], None)),
                    mock.patch.object(executor, "image_admission",
                                      lambda config, recipes: ([], None, False))):
        patcher.start()
        test.addCleanup(patcher.stop)


def config(root, repo, sha):
    doc = tomlfile.loads(read(os.path.join(LAB, "service/executor.example.toml")), "executor example")
    doc.update(root=root, repository_path=repo, control_sha=sha,
               sample_image=IMAGE, build_image=IMAGE, cycle_seconds=60, poll_main=False)
    doc["core"].update(shards=1, cases=["canonical_serialize_small"])
    doc["telemetry"].update(rounds=1, pairs=2, scale="smoke", corpus_runs=10)
    return contracts.validate_executor(doc)


def receipt(status="complete", measurement="fail"):
    return {"status": status, "measurement_status": measurement, "evidence": [],
            "actions": None, "detail": "synthetic orchestration test, no measurement claim"}


class ExecutorFixture(TempDir):
    def setUp(self):
        super().setUp()
        self.repo, self.base = lab_repo(self.path("repo"))
        self.head = self.repo.commit("candidate", {"README": "candidate"})
        self.c = config(self.path("journal"), self.repo.path, self.base)
        self.locks = self.path("locks")
        os.mkdir(self.locks)
        self.producer = mock.patch.object(executor, "producer", return_value=PRODUCER)
        self.producer.start()
        self.addCleanup(self.producer.stop)
        plenty_of_space(self)

    def submit(self, journal, work="core", key="manual_1", **kwargs):
        return journal.submit(work, self.base, self.head, key, **kwargs)

    def complete_core(self, j, request, calls, on_build=None, on_shard=None):
        """Run one queued core original through the real local path with synthetic builder,
        shards and filesystem stand-ins; `on_build(builder)` and `on_shard(options)` run first
        in each stand-in."""
        path = j.path(request["request_id"])
        plan = contracts.read_json(read(os.path.join(path, "plan.json")), "run")
        staged = synthetic.write_run(plan, self.path("synthetic_" + request["request_id"]),
            lambda slot, case: 200 if slot["side"] == "candidate" else 100)
        class Builder:
            def __init__(self, owner, plan, toolchain, **kw):
                self.layout = SimpleNamespace(build=os.path.join(owner, "target/performance/test/build"))
                self.plan = plan
                self.deadline_unix_ms = kw["deadline_unix_ms"]
            def build(self):
                calls.append("build")
                if on_build is not None:
                    on_build(self)
                os.makedirs(self.layout.build)
                return synthetic.builds(self.plan)
        def prepare(image, mount, size_bytes, **kwargs):
            calls.append("prepare")
            with open(image, "xb") as f:
                f.write(b"x")
            os.mkdir(mount)
        def shard(options, machine_lease=None):
            machine_lease.check()
            if on_shard is not None:
                on_shard(options)
            calls.append("shard_%d" % options.shard)
            shutil.copytree(os.path.join(staged, "shards", str(options.shard)),
                            os.path.join(options.run_dir, "shards", str(options.shard)))
            return contracts.read_file(os.path.join(options.run_dir, "shards", str(options.shard), "shard.json"), "shard")
        def cleanup(image, mount, **kwargs):
            calls.append("cleanup")
            os.rmdir(mount)
            os.unlink(image)
            return []
        with mock.patch.object(executor.builds, "Builder", Builder), \
                mock.patch.object(executor.hostprep, "prepare", prepare), \
                mock.patch.object(executor.hostprep, "cleanup", cleanup), \
                mock.patch.object(executor.runner, "run_shard", shard), \
                mock.patch.object(executor, "dispatch_actions", side_effect=AssertionError("network transport")), \
                mock.patch.dict(os.environ, {"HOME": self.path("private-home"), "TMPDIR": self.tmp}, clear=True):
            self.assertEqual(j.cycle(lock_directory=self.locks), "complete")
        return path, plan


class Journal(ExecutorFixture):
    def test_original_identity_and_frozen_backend_survive_switch_and_repeat(self):
        with executor.Journal(self.c) as j:
            first = self.submit(j)
            self.assertEqual(first, self.submit(j))
            changed = copy.deepcopy(self.c)
            changed["backend"] = "github_actions"
            j.config = changed
            self.assertEqual(self.submit(j), first)
            self.assertEqual(first["backend"], "local")
            self.assertEqual(len(j.names()), 1)
            with self.assertRaisesRegex(ContractError, "reused"):
                j.submit("core", self.base, self.base, "manual_1")
            observed = []
            def run(c, request, path, lease, installed):
                lease.check()
                self.assertEqual(c["backend"], "local")
                observed.append(request["request_id"])
                return receipt()
            self.assertEqual(j.cycle(execute=run, lock_directory=self.locks), "complete")
            self.assertEqual(j.cycle(execute=run, lock_directory=self.locks), "idle")
            self.assertEqual(observed, [first["request_id"]])
            result = contracts.read_json(read(os.path.join(j.path(first["request_id"]), "result.json")), "executor-result")
            self.assertFalse(result["required_check"])
            self.assertEqual(result["measurement_status"], "fail")
            self.assertIsNone(result["actions"])

    def test_same_journal_busy_is_bounded_and_cannot_consume_an_original(self):
        with executor.Journal(self.c) as journal:
            request = self.submit(journal)
            started = time.monotonic()
            with self.assertRaises(executor.JournalBusy):
                with executor.Journal(self.c):
                    self.fail("second controller")
            self.assertLess(time.monotonic() - started, 1)
            self.assertEqual(journal.state(request["request_id"])["state"], "queued")

    def test_busy_queue_is_unclaimed_then_runs_once_after_cleanup(self):
        with executor.Journal(self.c) as j:
            original = self.submit(j)
            with hostlock.MachineLock(self.locks) as held:
                held.guard()
                run = mock.Mock(side_effect=AssertionError("must not launch"))
                self.assertEqual(j.cycle(execute=run, lock_directory=self.locks), "busy")
                self.assertEqual(j.state(original["request_id"])["state"], "queued")
                self.assertIsNone(j.state(original["request_id"])["started_unix_ms"])
                run.assert_not_called()
                held.confirm_clean()
            self.assertEqual(j.cycle(execute=lambda *a: receipt(), lock_directory=self.locks), "complete")
            with hostlock.MachineLock(self.locks):
                pass

    def test_claim_crash_recovery_and_ambiguous_handoff_never_relaunch(self):
        with executor.Journal(self.c) as j:
            first = self.submit(j)
            j.transition(first, "claimed")
            j.recover()
            self.assertEqual(j.state(first["request_id"])["state"], "uncertain")
            self.assertEqual(j.cycle(execute=mock.Mock(side_effect=AssertionError("duplicate")),
                                     lock_directory=self.locks), "idle")
            hosted = copy.deepcopy(self.c)
            hosted["backend"] = "github_actions"
            hosted["core"]["environment"] = "linux_x86_64_gnu_hosted"
            hosted["telemetry"].update(scale=None, corpus_runs=None)
            j.config = hosted
            second = self.submit(j, key="hosted_1")
            calls = []
            def lost(workflow, payload):
                calls.append((workflow, payload))
                raise TimeoutError("accepted POST, lost response")
            with self.assertRaises(TimeoutError):
                j.cycle(transport=lost)
            self.assertEqual(j.state(second["request_id"])["state"], "uncertain")
            self.assertEqual(j.cycle(transport=lost), "idle")
            self.assertEqual(len(calls), 1)

    def test_hosted_dispatch_is_separate_and_has_no_invented_run_attempt(self):
        self.c["backend"] = "github_actions"
        self.c["core"]["environment"] = "linux_x86_64_gnu_hosted"
        self.c["telemetry"].update(scale=None, corpus_runs=None)
        with executor.Journal(self.c) as j:
            request = self.submit(j)
            local = mock.Mock(side_effect=AssertionError("hosted cannot compute locally"))
            calls = []
            self.assertEqual(j.cycle(execute=local, transport=lambda w, p: calls.append((w, p)) or "accepted"), "submitted")
            local.assert_not_called()
            self.assertEqual(calls[0][0], "performance.yml")
            result = contracts.read_json(read(os.path.join(j.path(request["request_id"]), "result.json")), "executor-result")
            self.assertEqual(result["backend"], "github_actions")
            self.assertNotIn("actions_run_id", result["actions"])
            self.assertNotIn("dispatch_head", result["actions"])
            self.assertEqual(result["actions"]["control_sha"], self.base)
            self.assertEqual(result["actions"]["requested_ref"], "refs/heads/main")
            self.assertEqual(calls[0][1]["inputs"]["head"], self.head)

    def test_hosted_declared_windows_refuse_before_any_original_or_transport(self):
        """performance.yml has no windows input, so a github_actions configuration declaring
        core.windows is refused at admission: no original is reserved and nothing is dispatched.
        Without windows the hosted dispatch still runs; local declared windows stay valid."""
        windows = {"shard_sampling_minutes": 20, "run_total_minutes": 110}
        local = copy.deepcopy(self.c)
        local["core"]["windows"] = dict(windows)
        self.assertEqual(contracts.validate_executor(local)["core"]["windows"], windows)
        hosted = copy.deepcopy(self.c)
        hosted.update(backend="github_actions")
        hosted["core"]["environment"] = "linux_x86_64_gnu_hosted"
        hosted["telemetry"].update(scale=None, corpus_runs=None)
        declared = copy.deepcopy(hosted)
        declared["core"]["windows"] = dict(windows)
        with self.assertRaisesRegex(ContractError, "hosted workflow cannot express declared windows"):
            contracts.validate_executor(copy.deepcopy(declared))
        with self.assertRaisesRegex(ContractError, "hosted workflow cannot express declared windows"):
            executor.Journal(copy.deepcopy(declared))
        self.assertFalse(os.path.exists(self.c["root"]))
        calls = []
        transport = lambda w, p: calls.append((w, p)) or "accepted"
        with executor.Journal(hosted) as j:
            with self.assertRaisesRegex(ContractError, "hosted workflow cannot express declared windows"):
                self.submit(j, config=copy.deepcopy(declared))
            self.assertEqual(j.names(), [])
            self.assertEqual(j.cycle(transport=transport), "idle")
            self.assertEqual(calls, [])
            self.submit(j)
            self.assertEqual(j.cycle(transport=transport), "submitted")
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], "performance.yml")
        self.assertEqual(sorted(calls[0][1]["inputs"]), ["authority", "base", "cases", "control",
                                                        "head", "scope", "shards", "tier"])

    def test_uncertain_cleanup_fences_every_later_workspace(self):
        with executor.Journal(self.c) as j:
            self.submit(j)
            def uncertain(c, request, path, lease, installed):
                lease.uncertain = True
                return receipt("uncertain", "interrupted")
            self.assertEqual(j.cycle(execute=uncertain, lock_directory=self.locks), "uncertain")
        with self.assertRaisesRegex(hostlock.Refused, "unfinished experiment"):
            hostlock.MachineLock(self.locks)
        self.assertTrue(os.path.isdir(os.path.join(self.locks, hostlock.MACHINE_LOCK_NAME + ".fence")))

    def test_cancellation_deadline_and_child_failure_keep_original_and_exclusion(self):
        for error in (KeyboardInterrupt("cancel"), budget.Interrupted("deadline"), RuntimeError("child failure")):
            with self.subTest(error=type(error).__name__):
                c = copy.deepcopy(self.c)
                c["root"] = self.path("journal_" + type(error).__name__)
                locks = self.path("locks_" + type(error).__name__)
                os.mkdir(locks)
                with executor.Journal(c) as j:
                    request = self.submit(j)
                    def failed(*args):
                        raise error
                    with self.assertRaises(type(error)):
                        j.cycle(execute=failed, lock_directory=locks)
                    self.assertEqual(j.state(request["request_id"])["state"], "uncertain")
                    self.assertEqual(j.cycle(execute=failed, lock_directory=locks), "idle")
                with self.assertRaises(hostlock.Refused):
                    hostlock.MachineLock(locks)

    def test_main_poll_observes_new_revisions_and_deduplicates_prior_defaults(self):
        with executor.Journal(self.c) as j:
            one = j.poll()
            self.assertEqual(j.poll(), one)
            new = self.repo.commit("new reviewed main", {"README": "new main"})
            two = j.poll()
            self.assertNotEqual(one["request_id"], two["request_id"])
            self.assertEqual(two["sources"]["candidate_sha"], new)
            self.assertEqual(two["sources"]["baseline_sha"], self.head)
            self.assertEqual(one["sources"]["candidate_sha"], self.head)

    def test_service_drains_full_queue_then_discovers_main_without_duplicate_claim(self):
        import cli
        self.c.update(max_queue=1, poll_main=True)
        with executor.Journal(self.c) as j:
            original = self.submit(j)
        latest = self.repo.commit("new reviewed main", {"README": "later tip"})
        calls = []
        def execute(c, request, path, lease, installed):
            lease.check()
            calls.append(request["request_id"])
            return receipt()
        native = executor.Journal.cycle
        def cycle(j):
            return native(j, lock_directory=self.locks)
        args = SimpleNamespace(operation="service", config="operator-config", work="core")
        with mock.patch.object(executor, "load_config", return_value=self.c), \
                mock.patch.object(executor.Journal, "cycle", cycle), \
                mock.patch.object(executor, "execute_local", execute):
            self.assertEqual(cli.cmd_executor(args), 0)
            with executor.Journal(self.c) as j:
                self.assertEqual(j.state(original["request_id"])["state"], "complete")
                main = [j.request(i) for i in j.names() if i != original["request_id"]]
                self.assertEqual(len(main), 1)
                self.assertEqual(main[0]["sources"]["candidate_sha"], latest)
                self.assertEqual(j.state(main[0]["request_id"])["state"], "queued")
            self.assertEqual(cli.cmd_executor(args), 0)
            self.assertEqual(cli.cmd_executor(args), 0)
        self.assertEqual(calls, [original["request_id"], main[0]["request_id"]])
        with executor.Journal(self.c) as j:
            self.assertEqual(len(j.names()), 2)
            self.assertTrue(all(j.state(i)["state"] == "complete" for i in j.names()))

    def test_service_busy_full_queue_waits_and_authority_errors_still_refuse(self):
        import cli
        self.c.update(max_queue=1, poll_main=True)
        with executor.Journal(self.c) as j:
            original = self.submit(j)
        native = executor.Journal.cycle
        def cycle(j):
            return native(j, lock_directory=self.locks)
        args = SimpleNamespace(operation="service", config="operator-config", work="core")
        with mock.patch.object(executor, "load_config", return_value=self.c), \
                mock.patch.object(executor.Journal, "cycle", cycle), \
                mock.patch.object(executor, "execute_local", return_value=receipt()), \
                mock.patch.object(executor.Journal, "poll", side_effect=ContractError("authority failure")) as poll:
            with hostlock.MachineLock(self.locks):
                self.assertEqual(cli.cmd_executor(args), 0)
                poll.assert_not_called()
            self.assertEqual(cli.cmd_executor(args), 4)
            poll.assert_called_once()
        with executor.Journal(self.c) as j:
            self.assertEqual(j.state(original["request_id"])["state"], "complete")

    def test_real_failed_child_and_deadline_settle_before_safe_subsequent_acquisition(self):
        for label, body, seconds in (("failed", "import sys; sys.exit(7)", 5),
                                     ("deadline", "import time; time.sleep(60)", .2)):
            with self.subTest(kind=label):
                c = copy.deepcopy(self.c)
                c["root"] = self.path("journal_" + label)
                observed = []
                def child(c, request, path, lease, installed):
                    work = os.path.join(path, "run")
                    os.mkdir(work)
                    now = time.monotonic()
                    run = telemetryexec.Execution([sys.executable, "-I", "-B", "-c", body],
                        {"PATH": "/usr/bin:/bin", "HOME": work, "TMPDIR": work}, work, [work], [],
                        work, "fixture", 65536, now + seconds, 65536, cleanup_deadline=now + seconds + 3)
                    outcome = telemetryexec.TrustedLocal(sys.executable).run(run)
                    observed.append(outcome)
                    self.assertEqual(outcome.cleanup, "confirmed", outcome.cleanup_detail)
                    return dict(receipt(measurement=label), evidence=executor.file_manifest(path, ["run"]))
                with executor.Journal(c) as j:
                    request = self.submit(j, key=label)
                    self.assertEqual(j.cycle(execute=child, lock_directory=self.locks), "complete")
                    self.assertEqual(j.state(request["request_id"])["state"], "complete")
                self.assertEqual(len(observed), 1)
                self.assertEqual(observed[0].timed_out, label == "deadline")
                if label == "failed":
                    self.assertEqual(observed[0].code, 7)
                with hostlock.MachineLock(self.locks):
                    pass

    def test_torn_publication_and_changed_config_and_bounds_refuse(self):
        with executor.Journal(self.c) as j:
            first = self.submit(j)
            with open(os.path.join(j.path(first["request_id"]), "state.json.pending"), "w") as f:
                f.write("torn")
            with self.assertRaisesRegex(ContractError, "torn"):
                j.recover()
            self.c["backend"] = "typo"
            with self.assertRaises(ContractError):
                contracts.validate_executor(self.c)
        self.c["backend"] = "local"
        self.c["root"] = self.path("bounded")
        self.c["max_requests"] = self.c["max_queue"] = 1
        with executor.Journal(self.c) as j:
            self.submit(j)
            with self.assertRaisesRegex(ContractError, "full"):
                self.submit(j, key="second")

    def test_removed_original_cannot_be_backfilled_or_hidden_from_calibration(self):
        with executor.Journal(self.c) as j:
            original = self.submit(j)
            shutil.rmtree(j.path(original["request_id"]))
            with self.assertRaisesRegex(ContractError, "membership changed"):
                self.submit(j)
            with self.assertRaisesRegex(ContractError, "membership changed"):
                j.cycle(execute=lambda *a: self.fail("replacement original"), lock_directory=self.locks)

    def run_synthetic_core(self, j, request):
        """Execute a submitted local core original with synthetic builds and shards whose
        candidate is twice as slow; returns the orchestration calls."""
        path = j.path(request["request_id"])
        plan = contracts.read_json(read(os.path.join(path, "plan.json")), "run")
        staged = synthetic.write_run(plan, self.path("synthetic-" + request["request_id"]),
            lambda slot, case: 200 if slot["side"] == "candidate" else 100)
        calls = []
        class Builder:
            def __init__(self, owner, plan, toolchain, **kw):
                self.layout = SimpleNamespace(build=os.path.join(owner, "target/performance/test/build"))
                self.plan = plan
            def build(self):
                calls.append("build")
                os.makedirs(self.layout.build)
                return synthetic.builds(self.plan)
        def prepare(image, mount, size_bytes, **kwargs):
            calls.append("prepare")
            with open(image, "xb") as f:
                f.write(b"x")
            os.mkdir(mount)
        def shard(options, machine_lease=None):
            machine_lease.check()
            calls.append("shard_%d" % options.shard)
            shutil.copytree(os.path.join(staged, "shards", str(options.shard)),
                            os.path.join(options.run_dir, "shards", str(options.shard)))
            return contracts.read_file(os.path.join(options.run_dir, "shards", str(options.shard), "shard.json"), "shard")
        def cleanup(image, mount, **kwargs):
            calls.append("cleanup")
            os.rmdir(mount)
            os.unlink(image)
            return []
        with mock.patch.object(executor.builds, "Builder", Builder), \
                mock.patch.object(executor.hostprep, "prepare", prepare), \
                mock.patch.object(executor.hostprep, "cleanup", cleanup), \
                mock.patch.object(executor.runner, "run_shard", shard), \
                mock.patch.object(executor, "dispatch_actions", side_effect=AssertionError("network transport")), \
                mock.patch.dict(os.environ, {"HOME": self.path("private-home"), "TMPDIR": self.tmp}, clear=True):
            self.assertEqual(j.cycle(lock_directory=self.locks), "complete")
        return calls

    def run_cancelled_core(self, j, request, cancel_at, cleanup_problems=()):
        """`run_synthetic_core` whose shard `cancel_at` consumed the controller's signal:
        the stand-in seals that shard's record and raises what `runner.run_shard` raises
        under a lease. Returns the orchestration calls and the cancellation that left."""
        path = j.path(request["request_id"])
        plan = contracts.read_json(read(os.path.join(path, "plan.json")), "run")
        staged = synthetic.write_run(plan, self.path("synthetic-" + request["request_id"]),
            lambda slot, case: 100)
        calls = []
        class Builder:
            def __init__(self, owner, plan, toolchain, **kw):
                self.layout = SimpleNamespace(build=os.path.join(owner, "target/performance/test/build"))
                self.plan = plan
            def build(self):
                calls.append("build")
                os.makedirs(self.layout.build)
                return synthetic.builds(self.plan)
        def prepare(image, mount, size_bytes, **kwargs):
            calls.append("prepare")
            with open(image, "xb") as f:
                f.write(b"x")
            os.mkdir(mount)
        def shard(options, machine_lease=None):
            machine_lease.check()
            calls.append("shard_%d" % options.shard)
            target = os.path.join(options.run_dir, "shards", str(options.shard))
            shutil.copytree(os.path.join(staged, "shards", str(options.shard)), target)
            doc = contracts.read_file(os.path.join(target, "shard.json"), "shard")
            if options.shard == cancel_at:
                # What `runner.run_shard` seals when the signal lands in the final slot
                # (`test_runner.ControllerCancellation`): every record kept, yet interrupted.
                doc.update(status="interrupted", failure={
                    "reason": "cancelled", "detail": "signal %d ended the shard after its last "
                    "slot ran; %d of %d slots recorded" % (signal.SIGTERM, doc["record_count"],
                                                           doc["expected_records"])})
                with open(os.path.join(target, "shard.json"), "wb") as stream:
                    stream.write(contracts.write_bytes(doc, "shard"))
                os.unlink(os.path.join(target, bundle.MANIFEST))
                bundle.write_manifest(target, plan["limits"]["shard_bundle_bytes"])
                raise runner.Cancelled(signal.SIGTERM, doc)
            return doc
        def cleanup(image, mount, **kwargs):
            calls.append("cleanup")
            if cleanup_problems:
                return list(cleanup_problems)
            os.rmdir(mount)
            os.unlink(image)
            return []
        with mock.patch.object(executor.builds, "Builder", Builder), \
                mock.patch.object(executor.hostprep, "prepare", prepare), \
                mock.patch.object(executor.hostprep, "cleanup", cleanup), \
                mock.patch.object(executor.runner, "run_shard", shard), \
                mock.patch.object(executor.compare, "evaluate", side_effect=AssertionError("verdict")), \
                mock.patch.dict(os.environ, {"HOME": self.path("private-home"), "TMPDIR": self.tmp}, clear=True):
            with self.assertRaises(runner.Cancelled) as raised:
                j.cycle(lock_directory=self.locks)
        return calls, raised.exception

    def test_a_cancelled_shard_settles_confirmed_cleanup_interrupted_and_never_replays(self):
        # A serial original fits one shard of this tier; later shards of one original, and
        # later originals, are `test_comparison`'s whole-controller cancellation.
        with executor.Journal(self.c) as j:
            request = self.submit(j)
            ident = request["request_id"]
            calls, cancelled = self.run_cancelled_core(j, request, cancel_at=0)
            self.assertEqual(calls, ["build", "prepare", "shard_0", "cleanup"],
                             "the cancelled original's scratch is still removed")
            self.assertEqual(j.state(ident)["state"], "interrupted")
            result = contracts.read_file(os.path.join(j.path(ident), "result.json"), "executor-result")
            self.assertEqual((result["status"], result["measurement_status"]),
                             ("interrupted", "interrupted"))
            self.assertIn("signal %d during shard 0" % signal.SIGTERM, result["detail"])
            self.assertIn("shards none and every later original were not started", result["detail"])
            paths = [item["path"] for item in result["evidence"]]
            self.assertIn("run/shards/0/shard.json", paths, "the sealed partial record is kept")
            sealed = contracts.read_file(os.path.join(j.path(ident), "run/shards/0/shard.json"),
                                         "shard")
            self.assertEqual((sealed["status"], sealed["record_count"], sealed["slots"]["unstarted"]),
                             ("interrupted", sealed["expected_records"], 0),
                             "every slot recorded, and the shard and original both interrupted")
            self.assertNotIn("report/report.html", paths, "a cancelled original has no verdict")
            self.assertEqual(cancelled.result["status"], "interrupted")
            # The confirmed cleanup settled the fence; the original never replays.
            hostlock.MachineLock(self.locks).release()
            self.assertEqual(j.cycle(execute=lambda *a: self.fail("replayed original"),
                                     lock_directory=self.locks), "idle")
            self.assertEqual(j.state(ident)["state"], "interrupted")

    def test_a_cancelled_shard_with_unconfirmed_cleanup_is_uncertain_and_keeps_the_fence(self):
        with executor.Journal(self.c) as j:
            request = self.submit(j)
            calls, cancelled = self.run_cancelled_core(j, request, cancel_at=0,
                                                       cleanup_problems=["umount exited 32"])
            self.assertEqual(calls, ["build", "prepare", "shard_0", "cleanup"])
            self.assertIsNone(cancelled.result)
            self.assertIn("umount exited 32", " ".join(getattr(cancelled, "__notes__", [])))
            self.assertEqual(j.state(request["request_id"])["state"], "uncertain")
            self.assertFalse(os.path.exists(os.path.join(j.path(request["request_id"]), "result.json")))
        with self.assertRaisesRegex(hostlock.Refused, "unfinished experiment"):
            hostlock.MachineLock(self.locks)

    def test_core_path_uses_retained_git_native_archive_and_reports_failed_verdict(self):
        """Real local orchestration; synthetic builder/shards and filesystem stand-ins."""
        with executor.Journal(self.c) as j:
            request = self.submit(j)
            calls = []
            path, plan = self.complete_core(j, request, calls)
            self.assertEqual(calls, ["build", "prepare", "shard_0", "cleanup"])
            original, state, result = executor.verify_original(j, request["request_id"])
            self.assertEqual(result["measurement_status"], "fail")
            # result/3 names the af each side measured, as the run's builds record has it.
            built = contracts.read_json(read(os.path.join(path, "run/builds.json")), "builds")
            self.assertEqual(result["schema"], "af.perf-executor-result/3")
            self.assertEqual(result["binaries"], dict((side, {
                "sha256": built["sides"][side]["artifacts"]["af"]["sha256"],
                "bytes": built["sides"][side]["artifacts"]["af"]["size"]}) for side in contracts.SIDES))
            self.assertNotEqual(result["binaries"]["baseline"], result["binaries"]["candidate"])
            self.assertEqual(result["backend"], "local")
            self.assertFalse(result["required_check"])
            self.assertTrue(os.path.isfile(os.path.join(path, "report/report.html")))
            with archive.Archive(os.path.join(self.c["root"], "core-archive")) as store:
                self.assertIn(plan["run_id"], store.state())
            # Original source objects survive deletion of the operator's moving mirror.
            shutil.rmtree(self.repo.path)
            executor.verify_original(j, request["request_id"])
            raw = os.path.join(path, "run/shards/0/samples.jsonl")
            with open(raw, "ab") as f:
                f.write(b"tampered")
            with self.assertRaisesRegex(ContractError, "changed"):
                executor.verify_original(j, request["request_id"])

    def test_overview_keeps_state_verification_coverage_and_verdict_apart(self):
        """The combined overview reads retained native state: a verified core with a real
        regression, a queued telemetry original and no profile is an incomplete comparison
        whose verdict still shows; tampered evidence is refused, never reported as measured."""
        import cli
        from perflab import overview
        with executor.Journal(self.c) as j:
            core = self.submit(j)
            self.run_synthetic_core(j, core)
            telemetry = self.submit(j, "telemetry", "telemetry_1")
            doc = overview.overview(j, [core["request_id"], telemetry["request_id"]],
                                    executor.verify_original, "full")
        self.assertEqual((doc["scope"], doc["status"]), ("full", "incomplete"))
        measured, queued = doc["members"]
        self.assertEqual(measured["verification"]["status"], "verified")
        self.assertEqual(measured["performance"]["verdict"], "fail")
        self.assertTrue(measured["performance"]["regressions"])
        coverage = measured["coverage"]
        self.assertEqual(coverage["observed"] + coverage["missing"], coverage["required"])
        self.assertIn("report/report.html", " ".join(measured["links"]))
        self.assertEqual((queued["state"], queued["verification"]["status"], queued["coverage"]),
                         ("queued", "not_terminal", None))
        self.assertIn("telemetry original is queued: no terminal result", doc["reasons"])
        for kind in ("trace", "cpu_profile"):
            self.assertIn("no %s profile original: its coverage is missing, not passed" % kind,
                          doc["reasons"])
        # Claiming completeness for this evidence is refused by the contract itself.
        with self.assertRaisesRegex(ContractError, "complete only when"):
            contracts.write_bytes(dict(doc, status="complete"), "comparison-overview")
        # Through the operator CLI: three renderings, exit 4 for an incomplete comparison.
        output = self.path("overview")
        with mock.patch.object(executor, "load_config", return_value=self.c), \
                mock.patch("sys.stdout", new=SimpleNamespace(buffer=io.BytesIO(), flush=lambda: None)):
            code = cli.main(["executor", "--config", "operator.toml", "overview", "--member",
                             core["request_id"], "--member", telemetry["request_id"],
                             "--scope", "full", "--output", output])
        self.assertEqual(code, 4)
        self.assertEqual(sorted(os.listdir(output)), sorted(overview.FILES))
        written = contracts.read_json(read(os.path.join(output, "overview.json")), "comparison-overview")
        self.assertEqual(written, doc)
        self.assertIn(b"Performance comparison: incomplete", read(os.path.join(output, "overview.html")))
        self.assertIn(b"| fail |", read(os.path.join(output, "overview.md")))
        with self.assertRaises(FileExistsError):
            overview.write(doc, output, self.c["root"])
        # Changed raw evidence: the member is refused and carries no coverage or verdict.
        with executor.Journal(self.c) as j:
            with open(os.path.join(j.path(core["request_id"]), "run/shards/0/samples.jsonl"), "ab") as f:
                f.write(b"tampered")
            refused = overview.overview(j, [core["request_id"]], executor.verify_original,
                                        "partial")
        member = refused["members"][0]
        self.assertEqual(member["verification"]["status"], "refused")
        self.assertEqual((member["coverage"], member["performance"]["verdict"]), (None, None))
        self.assertEqual(refused["status"], "incomplete")

    def test_overview_scope_health_and_diagnostic_affinity_decide_completeness(self):
        """A verified, fully covered core alone is at best a `partial` comparison; under the
        `full` scope it is incomplete without telemetry, trace and CPU profile originals; an
        `incomplete` core aggregate (a missing closing sentinel) is never complete; a profile
        whose parent is not a core original of the overview is not this comparison's diagnosis;
        and the contract refuses every relabelling of that evidence."""
        from perflab import overview
        with executor.Journal(self.c) as j:
            core = self.submit(j)
            self.run_synthetic_core(j, core)
            partial = overview.overview(j, [core["request_id"]], executor.verify_original,
                                        "partial")
            full = overview.overview(j, [core["request_id"]], executor.verify_original, "full")
        self.assertEqual((partial["status"], partial["reasons"]), ("partial", []))
        self.assertEqual(full["status"], "incomplete")
        for refusal in (dict(partial, status="complete"), dict(full, status="partial"),
                        dict(full, status="complete", reasons=[])):
            with self.assertRaisesRegex(ContractError, "complete only when|partial scope"):
                contracts.write_bytes(refusal, "comparison-overview")
        # The core aggregate's own health result decides eligibility, not its counts alone.
        with mock.patch.object(overview, "_core", lambda path: (
                {"required": 4, "observed": 4, "failed": 0, "missing": 0},
                {"verdict": "incomplete", "regressions": []})), executor.Journal(self.c) as j:
            unhealthy = overview.overview(j, [core["request_id"]], executor.verify_original,
                                          "partial")
        self.assertEqual(unhealthy["status"], "incomplete")
        self.assertIn("core aggregate result is incomplete", " ".join(unhealthy["reasons"]))
        # A complete full scope, with the four required originals, verifies; a profile of
        # another comparison's core does not count.
        members = [dict(full["members"][0], reasons=[], performance={"verdict": "fail",
                                                                     "regressions": []})]
        for index, (work, kind) in enumerate((("telemetry", None), ("profile", "trace"),
                                              ("profile", "cpu_profile"))):
            members.append(dict(members[0], request_id="exp_" + "%d" % index * 48, work=work,
                                diagnostic_kind=kind, plan_sha256=None,
                                parent=core["request_id"] if kind else None))
        complete = dict(full, status="complete", reasons=[], members=members)
        self.assertEqual(contracts.read_json(contracts.write_bytes(complete,
                                                                   "comparison-overview"),
                                             "comparison-overview")["status"], "complete")
        stray = copy.deepcopy(complete)
        stray["members"][3]["parent"] = "exp_" + "9" * 48
        with self.assertRaisesRegex(ContractError, "complete only when"):
            contracts.write_bytes(stray, "comparison-overview")

    def test_status_reads_the_journal_while_its_controller_holds_the_lock(self):
        """`status` and an overview take no lock: they read retained native state while the
        controller that holds the journal's exclusive lock executes."""
        import cli
        from perflab import overview
        with executor.Journal(self.c) as controller:
            request = self.submit(controller)
            with self.assertRaises(executor.JournalBusy):
                with executor.Journal(self.c):
                    pass
            with executor.Journal(self.c, readonly=True) as reader:
                self.assertEqual(reader.state(request["request_id"])["state"], "queued")
                doc = overview.overview(reader, [request["request_id"]],
                                        executor.verify_original, "partial")
            self.assertEqual(doc["status"], "incomplete")
            out = io.StringIO()
            with mock.patch.object(executor, "load_config", return_value=self.c), \
                    mock.patch("sys.stdout", new=out):
                code = cli.main(["executor", "--config", "operator.toml", "status"])
            self.assertEqual(code, 0)
            self.assertEqual(json.loads(out.getvalue())[0]["state"], "queued")

    def test_profile_selects_regressed_cases_of_its_verified_parent_with_configured_tools(self):
        """A profile without named cases diagnoses the regressions of a verified parent core
        original: its exact plan and verdict bytes, and the configured perf and Inferno."""
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
        self.c["diagnostic"].update(cases=[], max_cases=2, recipe="linux_gnu_cpu_profile",
                                    perf=os.path.join(tools, "perf"), inferno_bin=tools)
        paranoid = self.path("paranoid")
        with open(paranoid, "w") as f:
            f.write("2\n")
        with mock.patch.object(executor, "PERF_PARANOID", paranoid), executor.Journal(self.c) as j:
            core = self.submit(j)
            with self.assertRaisesRegex(ContractError, "not a completed local original"):
                self.submit(j, "profile", "early_profile", parent=core["request_id"])
            self.run_synthetic_core(j, core)
            with self.assertRaisesRegex(ContractError, "selects from a verified parent"):
                self.submit(j, "profile", "orphan_profile")
            with self.assertRaisesRegex(ContractError, "only a profile names a parent"):
                self.submit(j, "telemetry", "telemetry_parent", parent=core["request_id"])
            profile = self.submit(j, "profile", "profile_of_parent", parent=core["request_id"])
            parent_path = j.path(core["request_id"])
            verdict_bytes = read(os.path.join(parent_path, "report/verdict.json"))
            self.assertEqual(profile["parent"]["request_id"], core["request_id"])
            self.assertEqual(profile["parent"]["verdict_sha256"], sha256_bytes(verdict_bytes))
            self.assertEqual(profile["plan_sha256"], core["plan_sha256"])
            path = j.path(profile["request_id"])
            self.assertEqual(read(os.path.join(path, executor.PARENT_VERDICT)), verdict_bytes)
            # Resubmitting the key for another parent (or none) is refused, never rebound.
            with self.assertRaisesRegex(ContractError, "another parent"):
                self.submit(j, "profile", "profile_of_parent")
            seen = []
            class Builder:
                def __init__(self, owner, plan, toolchain, **kw):
                    self.layout = SimpleNamespace(build=os.path.join(owner, "target/performance/p/build"))
                def build(self):
                    seen.append("build")
                    os.makedirs(self.layout.build)
                    return {"cleanup": {"status": "removed"}}
            def profile_run(git, options, machine_lease=None):
                seen.append("profile")
                self.assertEqual((options.cases, options.max_cases), ([], 2))
                self.assertEqual(options.perf, os.path.join(tools, "perf"))
                self.assertEqual(options.inferno_bin, tools)
                # The native selection reads the frozen parent verdict against the exact plan.
                parent_plan = contracts.read_json(read(options.plan), "run")
                verdict = contracts.read_json(read(options.verdict), "verdict")
                selection, cases = diagnostics.select(parent_plan, verdict, options.cases,
                    options.max_cases, sha256_bytes(read(options.verdict)))
                self.assertEqual(selection["mode"], "automatic")
                self.assertTrue(cases and all(case["reason"] == "regressed"
                                              for case in selection["cases"]))
                return 0, None, "selection checked; nothing launched"
            def prepare(image, mount, size_bytes, **kwargs):
                os.mkdir(mount)
            def cleanup(image, mount, **kwargs):
                os.rmdir(mount)
                return []
            with mock.patch.object(executor.builds, "Builder", Builder), \
                    mock.patch.object(executor.hostprep, "prepare", prepare), \
                    mock.patch.object(executor.hostprep, "cleanup", cleanup), \
                    mock.patch.object(diagnostics, "run_profile", profile_run):
                self.assertEqual(j.cycle(lock_directory=self.locks), "complete")
            self.assertEqual(seen, ["build", "profile"])
            # A changed frozen parent verdict is refused at admission.
            with open(os.path.join(path, executor.PARENT_VERDICT), "ab") as f:
                f.write(b" ")
            with self.assertRaisesRegex(ContractError, "changed"):
                executor.verify_original(j, profile["request_id"])

    def test_profile_tools_are_refused_before_any_build(self):
        """Missing configured tools or a kernel denying every unprivileged perf event end a
        profile with its exact reason at submission and again before the first build."""
        control = self.repo.commit("diagnostic authority", {diagnostics.REGISTRY_PATH:
            read(os.path.join(ROOT, diagnostics.REGISTRY_PATH))})
        self.base = self.head = control
        self.c["control_sha"] = control
        self.c["diagnostic"]["recipe"] = "linux_gnu_cpu_profile"
        with executor.Journal(self.c) as j:
            with self.assertRaisesRegex(ContractError, "perf: not configured on this executor"):
                self.submit(j, "profile", "unconfigured")
            refused = j.preflight("profile", self.base, self.head, "unconfigured")
            self.assertEqual(refused["status"], "refused")
            tools = dict((tool["name"], tool) for tool in refused["tools"])
            self.assertEqual(sorted(tools), ["inferno_collapse_perf", "inferno_diff_folded",
                                             "inferno_flamegraph", "perf"])
            self.assertEqual((tools["perf"]["state"], tools["perf"]["detail"]),
                             ("unavailable", "not configured on this executor"))
            self.assertIn("perf: not configured on this executor", refused["detail"])
            contracts.write_bytes(refused, "executor-preflight")
            self.assertEqual(j.names(), [])
        tools = self.path("tools")
        os.mkdir(tools)
        for name in ("perf", "inferno-collapse-perf", "inferno-diff-folded", "inferno-flamegraph"):
            with open(os.path.join(tools, name), "w") as f:
                f.write("#!/bin/sh\nexit 0\n")
            os.chmod(os.path.join(tools, name), 0o755)
        self.c["diagnostic"].update(perf=os.path.join(tools, "perf"), inferno_bin=tools)
        paranoid = self.path("paranoid")
        with open(paranoid, "w") as f:
            f.write("4\n")
        with mock.patch.object(executor, "PERF_PARANOID", paranoid), executor.Journal(self.c) as j:
            with self.assertRaisesRegex(ContractError, "perf_event_paranoid=4 denies every perf"):
                self.submit(j, "profile", "paranoid")
            with open(paranoid, "w") as f:
                f.write("2\n")
            request = self.submit(j, "profile", "paranoid_later")
            # The host tightened after submission: nothing is built, the reason is exact.
            with open(paranoid, "w") as f:
                f.write("4\n")
            with mock.patch.object(executor.builds, "Builder",
                                   side_effect=AssertionError("built before tool check")):
                self.assertEqual(j.cycle(lock_directory=self.locks), "failed")
            state = j.state(request["request_id"])
            self.assertIn("refused before any build: kernel.perf_event_paranoid=4", state["detail"])

    def test_serial_local_schedule_is_planned_and_infeasible_work_refused_before_reservation(self):
        """The local backend runs shards one after another: a plan whose build and sampling
        windows cannot fit the run deadline is refused before any original exists. The full
        comparison recipe's declared windows (45 sampling, 120 run minutes, ADR-0269) are frozen
        into the plan; without them the ordinary nightly keeps its tier's 15 and 180."""
        self.c["core"].update(tier="nightly", environment="linux_x86_64_musl_claw", cases=[],
                              shards=4)
        self.c["zigbuild_bin"] = "/opt/pinned-tools/bin"
        with executor.Journal(contracts.validate_executor(self.c)) as j:
            nightly = j.preflight("core", self.base, self.head, "nightly_4")
            self.assertEqual(nightly["status"], "ready", nightly["detail"])
            self.assertEqual((nightly["serial"]["shard_sampling_seconds"],
                              nightly["serial"]["run_total_seconds"]), (15 * 60, 180 * 60))
        self.c["core"]["windows"] = {"shard_sampling_minutes": 45, "run_total_minutes": 120}
        with executor.Journal(self.c) as j:
            refused = j.preflight("core", self.base, self.head, "serial_4")
            contracts.write_bytes(refused, "executor-preflight")
            self.assertEqual(refused["status"], "refused")
            serial = refused["serial"]
            self.assertEqual((serial["shards"], serial["shard_sampling_seconds"],
                              serial["build_seconds"], serial["available_seconds"]),
                             (4, 45 * 60, 20 * 60, 120 * 60))
            self.assertFalse(serial["feasible"])
            self.assertEqual(serial["fitting_shards"], 2)
            self.assertIn("at most 2 shards fit", refused["detail"])
            with self.assertRaisesRegex(ContractError, "infeasible on the serial local backend"):
                self.submit(j, key="serial_4")
            self.assertEqual(j.names(), [])
        self.c["core"]["shards"] = 2
        with executor.Journal(self.c) as j:
            ready = j.preflight("core", self.base, self.head, "serial_2")
            contracts.write_bytes(ready, "executor-preflight")
            self.assertEqual(ready["status"], "ready", ready["detail"])
            self.assertEqual((ready["serial"]["shards"], ready["serial"]["needed_seconds"]),
                             (2, 20 * 60 + 2 * 45 * 60 + executor.runner.FINISH_RESERVE_SECONDS))
            self.assertEqual(j.names(), [], "a preflight reserves no original")
            request = self.submit(j, key="serial_2")
            self.assertEqual(len(j.names()), 1)
            plan = contracts.read_json(read(os.path.join(j.path(request["request_id"]),
                                                         "plan.json")), "run")
            self.assertEqual((plan["limits"]["shard_sampling_minutes"],
                              plan["limits"]["run_total_minutes"]), (45, 120))
            # The windows are frozen into the configuration and plan the original binds.
            self.assertEqual(request["configuration_digest"], canonical_digest(self.c))

    def test_preflight_submission_and_execution_share_time_and_storage_admission(self):
        """One calculation of the native reserve and of a telemetry original's sequential
        bounds (both builds' caps, scratch and its profile's timeout) for preflight, submission
        and execution: what preflight calls ready the claim admits, and an infeasible budget is
        refused before any original, claim or build, with its required and available values."""
        self.c["run_seconds"] = 3600
        with executor.Journal(self.c) as j:
            refused = j.preflight("telemetry", self.base, self.head, "short")
            contracts.write_bytes(refused, "executor-preflight")
            self.assertEqual(refused["status"], "refused")
            needed, parts = executor.time_bounds(self.c, "telemetry")
            self.assertEqual(dict(parts)["telemetry profile manual"], 240 * 60)
            self.assertEqual(dict(parts)["baseline build"], 4800)
            self.assertEqual(dict(parts)["candidate build"], 4800)
            self.assertIn("telemetry needs %d s" % needed, refused["detail"])
            self.assertIn("run_seconds allows 3600 s", refused["detail"])
            with self.assertRaisesRegex(ContractError, "refused before any claim or build"):
                self.submit(j, "telemetry", "short")
            self.assertEqual(j.names(), [], "a refusal reserves nothing")
        # The operator's 25200 s covers 2 x 4800 s builds, the 14400 s profile and the rest.
        self.c["run_seconds"] = 25200
        self.assertLessEqual(needed, 25200)
        self.c.update(archive_bytes=6 * 1024 ** 3, history_bytes=4 * 1024 ** 3)
        reserve = executor.native_reserve(self.c, "telemetry")
        # The 64 GiB bound the execution's reserve did not fit: refused at preflight now, with
        # the same figures the claim uses; a bound that holds it is ready.
        self.c["max_bytes"] = 64 * 1024 ** 3
        self.assertGreater(reserve, self.c["max_bytes"])
        with executor.Journal(self.c) as j:
            refused = j.preflight("telemetry", self.base, self.head, "storage")
            self.assertEqual(refused["status"], "refused")
            self.assertIn("%d-byte native reserve" % reserve, refused["detail"])
            self.assertIn("of max_bytes %d" % self.c["max_bytes"], refused["detail"])
            self.assertEqual(j.names(), [])
        self.c["max_bytes"] = 100 * 1024 ** 3
        with executor.Journal(self.c) as j:
            ready = j.preflight("telemetry", self.base, self.head, "storage")
            self.assertEqual(ready["status"], "ready", ready["detail"])
            request = self.submit(j, "telemetry", "storage")
            # Execution with less time left than both builds and the profile need: a clean
            # terminal refusal before either build, nothing measured and no fence kept.
            with mock.patch.object(executor, "TELEMETRY_BUILD_SECONDS", 25200), \
                    mock.patch.object(executor.telemetrybuild, "run",
                                      side_effect=AssertionError("built before admission")):
                self.assertEqual(j.cycle(lock_directory=self.locks), "failed")
            state = j.state(request["request_id"])
            self.assertIn("telemetry refused before any build, nothing measured", state["detail"])
            self.assertIn("of run_seconds 25200", state["detail"])
            result = contracts.read_json(read(os.path.join(j.path(request["request_id"]),
                                                           "result.json")), "executor-result")
            self.assertEqual(result["measurement_status"], "not_executed")
            self.assertFalse(os.path.lexists(os.path.join(self.locks,
                                                          "af-perf-machine.lock.fence")))

    def test_storage_admission_needs_the_policy_bound_and_the_filesystems_free_space(self):
        """A 100 GiB `max_bytes` is a bound, not space: preflight, submission and the claim
        each also need the root filesystem's available bytes for the same native reserve, and
        a large filesystem never lets the reserve exceed `max_bytes`."""
        self.c.update(run_seconds=25200, archive_bytes=6 * 1024 ** 3,
                      history_bytes=4 * 1024 ** 3, max_bytes=100 * 1024 ** 3)
        reserve = executor.native_reserve(self.c, "telemetry")
        free = {"bytes": reserve - 1}
        probed = []

        def available(path):
            probed.append(path)
            return free["bytes"]
        with mock.patch.object(executor.Journal, "free_bytes", staticmethod(available)), \
                executor.Journal(self.c) as j:
            refused = j.preflight("telemetry", self.base, self.head, "physical")
            self.assertEqual(refused["status"], "refused")
            self.assertIn("too little free space", refused["detail"])
            self.assertIn("filesystem: %d bytes available for the %d-byte native reserve"
                          % (reserve - 1, reserve), refused["detail"])
            self.assertIn("of max_bytes %d" % self.c["max_bytes"], refused["detail"])
            self.assertEqual(set(probed), {self.c["root"]})
            with self.assertRaisesRegex(ContractError, "too little free space"):
                self.submit(j, "telemetry", "physical")
            self.assertEqual(j.names(), [], "a refusal reserves nothing")
            free["bytes"] = reserve
            ready = j.preflight("telemetry", self.base, self.head, "physical")
            self.assertEqual(ready["status"], "ready", ready["detail"])
            request = self.submit(j, "telemetry", "physical")
            # Space taken between submission and the claim: the claim refuses before anything
            # is claimed or built, and the original stays queued, never pruned or replayed.
            free["bytes"] = reserve - 1
            with mock.patch.object(executor.telemetrybuild, "run",
                                   side_effect=AssertionError("built before admission")):
                with self.assertRaisesRegex(ContractError, "too little free space"):
                    j.cycle(lock_directory=self.locks)
            self.assertEqual(j.state(request["request_id"])["state"], "queued")
        # A filesystem far larger than the policy never widens it.
        free["bytes"] = 1 << 50
        self.c["max_bytes"] = reserve - 1
        with mock.patch.object(executor.Journal, "free_bytes", staticmethod(available)), \
                executor.Journal(self.c) as j:
            refused = j.preflight("telemetry", self.base, self.head, "bounded")
            self.assertEqual(refused["status"], "refused")
            self.assertIn("capacity exhausted", refused["detail"])
    def rebind(self, j, ident, request=None, result=None):
        """Rewrite an original's receipts as an earlier writer or a forger would, rebinding
        every digest the journal holds so only the reader's own checks remain."""
        path = j.path(ident)
        if request is not None:
            with open(os.path.join(path, "request.json"), "wb") as f:
                f.write(document_bytes(request))
            digest = canonical_digest(request)
            j.index["originals"] = [dict(item, request_digest=digest) if item["request_id"] == ident
                                    else item for item in j.index["originals"]]
            with open(os.path.join(j.root, "journal.json"), "wb") as f:
                f.write(document_bytes(j.index))
            result = dict(result, request_digest=digest)
        with open(os.path.join(path, "result.json"), "wb") as f:
            f.write(document_bytes(result))
        state = json.loads(read(os.path.join(path, "state.json")))
        state.update(request_digest=result["request_digest"],
                     result_sha256=sha256_bytes(document_bytes(result)))
        with open(os.path.join(path, "state.json"), "wb") as f:
            f.write(document_bytes(state))

    def test_old_journal_request_v1_and_result_v2_still_verify_and_report_status(self):
        import cli
        with executor.Journal(self.c) as j:
            request = self.submit(j)
            self.complete_core(j, request, [])
            ident = request["request_id"]
            _, _, result = executor.verify_original(j, ident)
            forged = copy.deepcopy(result)
            forged["binaries"]["candidate"]["bytes"] += 1
            self.rebind(j, ident, result=forged)
            with self.assertRaisesRegex(ContractError, "binaries are not the ones"):
                executor.verify_original(j, ident)
            # What the previous release wrote: request/1, result/2 without binaries. Neither
            # earlier request version froze a parent or partition (request/3, ADR-0269).
            old_request = dict(request, schema="af.perf-executor-request/1")
            del old_request["parent"], old_request["partition"]
            old_result = dict(result, schema="af.perf-executor-result/2")
            del old_result["binaries"]
            # The current shape under an earlier version is refused, never read as both.
            for schema in ("af.perf-executor-request/1", "af.perf-executor-request/2"):
                self.rebind(j, ident, request=dict(request, schema=schema), result=old_result)
                with self.assertRaisesRegex(ContractError, "unknown field parent, partition"):
                    executor.verify_original(j, ident)
            self.rebind(j, ident, request=old_request, result=old_result)
            original, state, verified = executor.verify_original(j, ident)
            self.assertEqual((original["schema"], verified["schema"], state["state"]),
                             ("af.perf-executor-request/1", "af.perf-executor-result/2", "complete"))
            self.assertNotIn("binaries", verified)
            # The same submission is still the same original, never a second one.
            self.assertEqual(self.submit(j), old_request)
            with self.assertRaises(ContractError):
                contracts.write_bytes(verified, "executor-result")
        output = io.BytesIO()
        with mock.patch.object(executor, "load_config", return_value=self.c), \
                mock.patch.object(sys, "stdout", io.TextIOWrapper(output, encoding="utf-8")) as stdout:
            self.assertEqual(cli.main(["executor", "--config", "operator.toml", "status"]), 0)
            self.assertEqual(cli.main(["executor", "--config", "operator.toml", "verify",
                                       "--request-id", ident]), 0)
            stdout.flush()
        status, printed = output.getvalue().decode().splitlines()
        self.assertEqual([item["state"] for item in json.loads(status)], ["complete"])
        self.assertEqual(json.loads(printed)["schema"], "af.perf-executor-result/2")

    def test_existing_kinds_keep_their_submission_and_identity(self):
        with executor.Journal(self.c) as j:
            manual = self.submit(j)
        self.assertEqual(manual["submission"], {"kind": "manual", "key": "manual_1"})
        self.assertEqual(manual["request_id"], "exp_" + canonical_digest(
            {"submission": {"kind": "manual", "key": "manual_1"}, "work": "core"})[7:55])

    def test_local_diagnostic_uses_native_child_planner_report_and_admission(self):
        control = self.repo.commit("diagnostic authority", {diagnostics.REGISTRY_PATH:
            read(os.path.join(ROOT, diagnostics.REGISTRY_PATH))})
        self.base = self.head = control
        self.c["control_sha"] = control
        calls = []
        class Builder:
            def __init__(self, owner, plan, toolchain, **kwargs):
                self.layout = SimpleNamespace(build=os.path.join(owner, "target/performance/build"))
                self.plan = plan
            def build(self):
                calls.append("build")
                os.makedirs(self.layout.build)
                return synthetic.builds(self.plan)
        def prepare(image, mount, size_bytes, **kwargs):
            with open(image, "xb") as f:
                f.write(b"synthetic filesystem image")
            os.makedirs(os.path.join(mount, "sample"))
        def cleanup(image, mount, **kwargs):
            self.assertEqual(os.listdir(os.path.join(mount, "sample")), [])
            os.rmdir(os.path.join(mount, "sample"))
            os.rmdir(mount)
            os.unlink(image)
            calls.append("cleanup")
            return []
        native = diagnostics.run_profile
        def profile(git, options, machine_lease=None):
            self.assertEqual(options.mode, "linux_container")
            self.assertEqual(options.image, IMAGE)
            machine_lease.check()
            calls.append("profile")
            # No product/tool is executed. The actual native controller produces a complete
            # diagnostic refusal bundle with both sides' missing-build omissions.
            options.parent_build = options.build = options.builder = None
            options.contained_host = lambda *a: (options.scratch_fs, [0, 1], "synthetic runtime")
            suite, golden = executor.runner.oracle.load(os.path.join(ROOT, "fixtures/performance/suite.toml"))
            return native(git, options, authority_factory=lambda p: (suite, golden), machine_lease=machine_lease)
        with executor.Journal(self.c) as j:
            request = self.submit(j, "profile", "profile_1")
            with mock.patch.object(executor.builds, "Builder", Builder), \
                    mock.patch.object(executor.hostprep, "prepare", prepare), \
                    mock.patch.object(executor.hostprep, "cleanup", cleanup), \
                    mock.patch.object(diagnostics, "run_profile", profile):
                self.assertEqual(j.cycle(lock_directory=self.locks), "complete")
            self.assertEqual(calls, ["build", "profile", "cleanup"])
            _, _, result = executor.verify_original(j, request["request_id"])
            self.assertEqual(result["measurement_status"], "unavailable")
            self.assertFalse(os.path.exists(os.path.join(j.path(request["request_id"]), "run")),
                             "diagnostics never create verdict samples")
            self.assertTrue(os.path.isfile(os.path.join(j.path(request["request_id"]), "profile/diagnostic.html")))

    def test_shipping_nightly_release_reach_both_builders_with_frozen_pinned_tools(self):
        control = self.repo.commit("diagnostic authority", {diagnostics.REGISTRY_PATH:
            read(os.path.join(ROOT, diagnostics.REGISTRY_PATH))})
        self.base = self.head = control
        self.c["control_sha"] = control
        tool_dir = "/opt/pinned-tools/bin"
        native_builder, native_profile = builds.Builder, diagnostics.run_profile
        class PreflightReached(ContractError):
            pass
        for tier in ("nightly", "release"):
            for work in ("core", "profile"):
                with self.subTest(tier=tier, work=work):
                    c = copy.deepcopy(self.c)
                    c.update(root=self.path(tier + work), zigbuild_bin=tool_dir)
                    c["core"].update(tier=tier, environment="linux_x86_64_musl_claw",
                                     cases=["cli_discovery__command_version"])
                    c["diagnostic"]["cases"] = ["cli_discovery__command_version"]
                    c["diagnostic"]["recipe"] = "linux_musl_trace"
                    locks = self.path(tier + work + "-locks")
                    os.mkdir(locks)
                    checked, tool_calls = [], []
                    class Image:
                        env = {}
                        def __init__(inner, docker, reference):
                            self.assertEqual(reference, IMAGE)
                            inner.reference = reference
                        def refuse_implicit_volumes(inner):
                            pass  # Image admission alone is substituted; tool readers are real.
                        def run_once(inner, argv, env):
                            tool_calls.append((argv, env))
                            if argv == ["rustc", "--print", "sysroot"]:
                                out = b"/pinned/rust"
                            elif argv == ["/pinned/rust/bin/rustc", "-vV"]:
                                out = b"rustc 1.88.0\nrelease: 1.88.0\nhost: x86_64-unknown-linux-gnu\n"
                            elif argv == ["/pinned/rust/bin/cargo", "-V"]:
                                out = b"cargo 1.88.0"
                            elif argv == [tool_dir + "/cargo-zigbuild", "--version"]:
                                self.assertEqual(env["PATH"].split(":")[0], tool_dir)
                                out = b"cargo-zigbuild 0.23.4"
                            elif argv[-2:] == ["sh", tool_dir]:
                                out = b"zig\n0.15.2\n"
                            else:
                                self.assertEqual(argv[:2], ["/bin/sh", "-c"])
                                self.assertIn("--print target-libdir", argv[2])
                                out = b""
                            return subprocess.CompletedProcess(argv, 0, out, b"")
                    class Builder(native_builder):
                        def build(inner):
                            inner.budget.free_bytes = lambda path: 1 << 50
                            inner.check_host()  # Real recipe dispatch, Rust and Zig version checks.
                            self.assertTrue(inner.bounded_container)
                            self.assertEqual(inner.zigbuild_bin, tool_dir)
                            self.assertIn("cargo-zigbuild 0.23.4; zig 0.15.2", inner.zigbuild.identity)
                            checked.append(inner.plan["scope"])
                            if work == "core" or inner.plan["scope"] == "diagnostic":
                                raise PreflightReached("preflight reached; no compilation or measurement")
                            os.makedirs(inner.layout.build)
                            return synthetic.builds(inner.plan)
                    def profile(git, options, machine_lease=None):
                        options.contained_host = lambda *a: (options.scratch_fs, [0, 1], "synthetic runtime")
                        return native_profile(git, options, machine_lease=machine_lease)
                    with executor.Journal(c) as j:
                        request = self.submit(j, work, tier + work)
                        frozen = contracts.read_json(read(os.path.join(j.path(request["request_id"]),
                                                                      "configuration.json")), "executor")
                        self.assertEqual(frozen["zigbuild_bin"], tool_dir)
                        j.config = dict(c, zigbuild_bin="/other/operator-setting")
                        with mock.patch.object(builds, "Builder", Builder), \
                                mock.patch.object(builds, "BuildImage", Image), \
                                mock.patch.object(executor.hostprep, "prepare", side_effect=lambda i, p, *a, **kw: os.mkdir(p)), \
                                mock.patch.object(diagnostics, "run_profile", profile), \
                                self.assertRaises(PreflightReached):
                            j.cycle(lock_directory=locks)
                    self.assertEqual(checked, ["partial_scope"] + (["diagnostic"] if work == "profile" else []))
                    self.assertEqual(sum(argv == [tool_dir + "/cargo-zigbuild", "--version"]
                                         for argv, env in tool_calls), len(checked))

    def test_diagnostic_build_removal_uncertainty_keeps_borrowed_executor_fence(self):
        control = self.repo.commit("diagnostic authority", {diagnostics.REGISTRY_PATH:
            read(os.path.join(ROOT, diagnostics.REGISTRY_PATH))})
        self.base = self.head = control
        self.c["control_sha"] = control
        calls = []
        class Builder:
            def __init__(self, owner, plan, toolchain, **kwargs):
                self.layout = SimpleNamespace(build=os.path.join(owner, "target/performance/build"))
                self.plan = plan
            def build(inner):
                calls.append("build")
                if len(calls) == 1:
                    os.makedirs(inner.layout.build)
                    return synthetic.builds(inner.plan)
                image = "sha256:" + "a" * 64
                docker = build_fixtures.fake_docker(self.path("docker"), {image: (image, [])},
                                                   lingering="af-perf-probe-")
                builds.BuildImage(docker, image).run_once(["/bin/true"])
                self.fail("removal must refuse")
        def prepare(image, mount, size_bytes, **kwargs):
            os.mkdir(mount)
        native = diagnostics.run_profile
        borrowed = []
        def profile(git, options, machine_lease=None):
            borrowed.append(machine_lease)
            options.contained_host = lambda *a: (options.scratch_fs, [0, 1], "synthetic runtime")
            return native(git, options, machine_lease=machine_lease)
        # Tool checks only before the first build (the executor's own pre-build check): none
        # after the diagnostic build's container removal became uncertain.
        probed = []
        real_tools = diagnostics.probe_tools

        def checked_tools(*args, **kwargs):
            probed.append(len(calls))
            return real_tools(*args, **kwargs)
        with executor.Journal(self.c) as j:
            request = self.submit(j, "profile", "uncertain-profile")
            path = j.path(request["request_id"])
            with mock.patch.object(builds, "Builder", Builder), \
                    mock.patch.object(executor.hostprep, "prepare", prepare), \
                    mock.patch.object(executor.hostprep, "cleanup") as cleanup, \
                    mock.patch.object(diagnostics, "run_profile", profile), \
                    mock.patch.object(diagnostics, "probe_tools", checked_tools):
                with self.assertRaises(diagnostics.Unavailable) as caught:
                    j.cycle(lock_directory=self.locks)
            self.assertEqual(caught.exception.reason, "build_container_unconfirmed")
            self.assertFalse(caught.exception.cleanup_confirmed)
            self.assertTrue(borrowed[0].uncertain)
            cleanup.assert_not_called()
            self.assertEqual(probed, [0], "no tool is probed after a build began")
            self.assertEqual(calls, ["build", "build"])
            self.assertEqual(j.state(request["request_id"])["state"], "uncertain")
            self.assertIn(b"af-perf-probe-", read(os.path.join(path, "profile/cleanup-uncertain.txt")))
            self.assertTrue(os.path.isdir(os.path.join(path, "scratch")))
            self.assertEqual(j.cycle(lock_directory=self.locks), "idle")
        with self.assertRaisesRegex(hostlock.Refused, "unfinished experiment"):
            hostlock.MachineLock(self.locks)

    def test_local_telemetry_builds_both_sides_verifies_raw_and_keeps_separate_history(self):
        self.run_local_telemetry(IMAGE)

    def test_local_telemetry_publishes_and_verifies_an_actual_local_image_id(self):
        """The image ID a full comparison's build admission took (ADR-0170) is the build
        record's image as is: published by the executor after both builds and verified with
        the native result, never rewritten into, or equated with, a manifest digest."""
        records = self.run_local_telemetry(LOCAL_IMAGE_ID)
        self.assertEqual([r["image"] for r in records], [LOCAL_IMAGE_ID] * 2)

    def test_telemetry_build_records_refuse_previous_versions_and_mutable_references(self):
        binary = self.path("af")
        with open(binary, "wb") as f:
            f.write(b"stand-in binary, never executed")
        os.chmod(binary, 0o500)
        cli = runpy.run_path(os.path.join(LAB, "cli.py"), run_name="af_build_record_test_cli")

        def record(image, output):
            stdout = io.TextIOWrapper(io.BytesIO(), encoding="utf-8")
            with mock.patch.object(sys, "stdout", stdout), mock.patch.object(sys, "stderr", io.StringIO()):
                code = cli["main"](["telemetry-build-record", "--side", "candidate",
                    "--source-sha", self.head, "--binary", binary, "--image", image,
                    "--rustc", "rustc 1.88.0", "--cargo", "cargo 1.88.0",
                    "--command", "cargo build --release --locked --bin af", "--output", output])
                stdout.flush()
                return code, stdout.buffer.getvalue()

        written = self.path("local.json")
        self.assertEqual(record(LOCAL_IMAGE_ID, written), (0, b""))
        doc = telemetrylab.read_build(written, "candidate", self.head, sha256_bytes(read(binary)))
        self.assertEqual((doc["schema"], doc["image"]), ("af.perf-telemetry-build/2", LOCAL_IMAGE_ID))
        for image in ("registry.invalid/sample:latest", "registry.invalid/sample:1.88@sha256:" + "1" * 64,
                      "Registry.invalid/Sample@sha256:" + "1" * 64, "sha256:" + "4f5e1e215d31",
                      "sha256:" + "A" * 64, "4f5e1e215d31e1e7844332afa5c4585bc63cdad88ae6e0b9237ffdbdeefbc2e1"):
            with self.subTest(image=image):
                output = self.path("refused-%d.json" % len(os.listdir(self.tmp)))
                code, out = record(image, output)
                self.assertEqual(code, 2)
                self.assertEqual(json.loads(out)["reason"], "invalid_telemetry_build")
                self.assertFalse(os.path.exists(output), "nothing is written for a refused image")
        # The previous version fails closed, whatever image it names.
        old = self.path("old.json")
        with open(old, "wb") as f:
            f.write(json.dumps(dict(doc, schema="af.perf-telemetry-build/1", image=IMAGE)).encode())
        with self.assertRaisesRegex(ContractError, "expected af.perf-telemetry-build/2"):
            telemetrylab.read_build(old, "candidate", self.head, doc["binary_sha256"])

    def run_local_telemetry(self, build_image):
        """One local telemetry original through the real executor, both builds and their
        records, the native result and its verification; the build records it published."""
        self.c["core"]["cases"] = ["canonical_serialize_small"]
        self.c["build_image"] = contracts.validate_executor(dict(self.c, build_image=build_image))["build_image"]
        with executor.Journal(self.c) as j:
            request = self.submit(j, "telemetry", "telemetry_1")
            seen = []
            def build(source, output, reference, scratch_fs, source_sha, seconds):
                seen.append((source_sha, reference, seconds))
                os.mkdir(output)
                for name, data in (("af", b"stand-in binary, never executed"),
                                   ("af-rustc", b"synthetic rustc"), ("af-cargo", b"synthetic cargo")):
                    with open(os.path.join(output, name), "wb") as f:
                        f.write(data)
                os.chmod(os.path.join(output, "af"), 0o500)
            def prepare(image, mount, size_bytes, **kwargs):
                with open(image, "xb") as f:
                    f.write(b"x")
                os.mkdir(mount)
            def cleanup(image, mount, *args, **kwargs):
                os.rmdir(mount)
                os.unlink(image)
                return []
            native_run = telemetrylab.run
            observed = []
            def run(settings, machine_lease=None):
                self.assertEqual(settings.mode, "linux_container")
                self.assertEqual(set(settings.builds), {"baseline", "candidate"})
                self.assertEqual(settings.image, IMAGE)
                observed.append(settings)
                binaries = {"baseline": settings.baseline_af, "candidate": settings.candidate_af}
                settings.executor = tf.StandIn(binaries)
                settings.host_identity = lambda: tf.HOST
                settings.free_bytes = lambda path: 1 << 40
                return native_run(settings, machine_lease=machine_lease)
            with mock.patch.object(executor.telemetrybuild, "run", build), \
                    mock.patch.object(executor.hostprep, "prepare", prepare), \
                    mock.patch.object(executor.hostprep, "cleanup", cleanup), \
                    mock.patch.object(telemetrylab, "run", run), \
                    mock.patch.object(executor, "dispatch_actions", side_effect=AssertionError("Actions transport")):
                self.assertEqual(j.cycle(lock_directory=self.locks), "complete")
            self.assertEqual([i[0] for i in seen], [self.base, self.head])
            self.assertEqual(len(observed), 1)
            self.assertTrue(all(i[1] == build_image and 30 < i[2] <= 4800 for i in seen))
            _, _, result = executor.verify_original(j, request["request_id"])
            self.assertEqual(result["measurement_status"], "complete")
            history_path = os.path.join(self.c["root"], "local-telemetry-history/history.json")
            hist = contracts.read_json(read(history_path), "telemetry-history")
            self.assertEqual(len(hist["results"]), 1)
            self.assertTrue(hist["results"][0]["result_id"].startswith("local_"))
            self.assertFalse(os.path.exists(os.path.join(self.c["root"], "hosted-telemetry-history")))
            path = j.path(request["request_id"])
            measured, _, _ = telemetrylab.verify(os.path.join(path, "run"), executor.ROOT)
            records = [contracts.read_file(os.path.join(path, side + "-build", "build.json"),
                                           "telemetry-build") for side in contracts.SIDES]
            self.assertEqual([measured["binaries"][side]["build"] for side in contracts.SIDES],
                             records, "the verified result carries the published records")
            return records


class Nightly(ExecutorFixture):
    """`submit --kind nightly` through the real CLI, journal and Git history (ADR-0266)."""
    TAG = "nightly-2026.10.29"

    def setUp(self):
        super().setUp()
        self.c["core"].update(tier="nightly", environment="linux_x86_64_musl_claw",
                              cases=["cli_discovery__command_version"])
        self.c["zigbuild_bin"] = "/opt/af-perf-tools/bin"

    def release_history(self):
        """lab (v0.12.0) - v0.13.0 - v0.14.0-rc.3 - main tip (the nightly's frozen commit), and
        a later stable v0.14.0 on a side branch, which the tip cannot reach."""
        self.repo.tag("v0.12.0", self.base)
        self.stable = self.repo.commit("release 0.13.0", {"README": "0.13.0"})
        self.repo.tag("v0.13.0", self.stable, annotated=True)
        self.rc = self.repo.commit("release candidate", {"README": "0.14.0-rc.3"})
        self.repo.tag("v0.14.0-rc.3", self.rc)
        self.repo.git("checkout", "-q", "-b", "side")
        self.repo.tag("v0.14.0", self.repo.commit("unreachable release", {"README": "side"}))
        self.repo.git("checkout", "-q", "main")
        self.head = self.repo.commit("frozen main", {"README": "nightly"})
        self.repo.tag(self.TAG, self.head)

    def cli(self, *argv):
        """(exit status, stdout, stderr) of `cli.py executor ... submit --kind nightly`."""
        import cli
        output, errors = io.BytesIO(), io.StringIO()
        stdout = io.TextIOWrapper(output, encoding="utf-8")
        with mock.patch.object(executor, "load_config", return_value=self.c), \
                mock.patch.object(sys, "stdout", stdout), mock.patch.object(sys, "stderr", errors):
            code = cli.main(["executor", "--config", "operator.toml", "submit", "--kind", "nightly"]
                            + list(argv))
            stdout.flush()
        return code, output.getvalue().decode(), errors.getvalue()

    def originals(self):
        with executor.Journal(self.c) as j:
            return [j.request(ident) for ident in j.names()]

    def test_nightly_core_derives_the_highest_reachable_stable_release_and_is_idempotent(self):
        self.release_history()
        code, out, _ = self.cli("--work", "core", "--tag", self.TAG, "--head", self.head)
        self.assertEqual(code, 0)
        request = json.loads(out)
        # request/3: a nightly is neither a comparison partition nor a parent-selected profile.
        self.assertEqual((request["schema"], request["parent"], request["partition"]),
                         ("af.perf-executor-request/3", None, None))
        self.assertEqual(request["submission"], {"kind": "nightly", "key": "nightly:%s:core" % self.TAG,
            "tag": self.TAG, "baseline": {"tag": "v0.13.0", "sha": self.stable}})
        self.assertEqual((request["sources"]["baseline_sha"], request["sources"]["candidate_sha"]),
                         (self.stable, self.head))
        plan = contracts.read_json(read(os.path.join(self.c["root"], "requests", request["request_id"],
                                                     "plan.json")), "run")
        self.assertEqual((plan["tier"], plan["profile"]["environment_id"]),
                         ("nightly", "linux_x86_64_musl_claw"))
        # The same night again is the same original.
        code, again, _ = self.cli("--work", "core", "--tag", self.TAG, "--head", self.head)
        self.assertEqual((code, json.loads(again)), (0, request))
        # Another head under the same tag is a reused key, not a second original.
        later = self.repo.commit("later main", {"README": "later"})
        code, _, errors = self.cli("--work", "core", "--tag", self.TAG, "--head", later)
        self.assertEqual(code, 4)
        self.assertIn("reused for another source", errors)
        # Telemetry of the same night is its own original; an explicit --base is recorded as such.
        code, out, _ = self.cli("--work", "telemetry", "--tag", self.TAG, "--head", self.head,
                                "--base", self.rc)
        self.assertEqual(code, 0)
        telemetry = json.loads(out)
        self.assertEqual(telemetry["submission"], {"kind": "nightly", "key": "nightly:%s:telemetry" % self.TAG,
                                                   "tag": self.TAG, "baseline": None})
        self.assertEqual(telemetry["sources"]["baseline_sha"], self.rc)
        self.assertEqual(sorted(r["request_id"] for r in self.originals()),
                         sorted([request["request_id"], telemetry["request_id"]]))

    def test_nightly_refusals_reserve_no_original(self):
        quick = copy.deepcopy(self.c)
        quick["core"].update(tier="quick", environment="linux_x86_64_gnu_claw", cases=[])
        quick["zigbuild_bin"] = None
        nightly, self.c = self.c, quick
        code, out, errors = self.cli("--work", "core", "--tag", self.TAG, "--head", self.head)
        self.assertEqual(code, 4)
        self.assertIn('tier is "quick"', errors)
        self.assertEqual(json.loads(out)["reason"], "executor_refused")
        self.c = nightly
        for argv, message in (
                (["--work", "profile", "--tag", self.TAG, "--head", self.head], "never --work profile"),
                (["--work", "core", "--tag", "v0.13.0", "--head", self.head], "nightly-YYYY.MM.DD"),
                (["--work", "core", "--tag", "nightly-2026.02.30", "--head", self.head], "no calendar date"),
                (["--work", "core", "--head", self.head], "needs --tag and --head"),
                (["--work", "core", "--tag", self.TAG, "--head", self.head, "--key", "manual"], "no --key")):
            with self.subTest(argv=argv):
                code, _, errors = self.cli(*argv)
                self.assertEqual(code, 4)
                self.assertIn(message, errors)
        # A frozen main with no stable release behind it has no baseline to derive.
        orphan = self.repo.git("commit-tree", "-m", "unreleased", self.repo.git("rev-parse", "HEAD^{tree}"))
        code, _, errors = self.cli("--work", "telemetry", "--tag", self.TAG, "--head", orphan)
        self.assertEqual(code, 4)
        self.assertIn("no stable release tag is reachable", errors)
        self.assertEqual(self.originals(), [])

    def test_nightly_freezes_the_head_its_baseline_was_derived_from_when_the_ref_moves(self):
        self.repo.tag("v0.12.0", self.base)
        derive = gitrefs.nightly_baseline

        def derive_then_advance(git, head):
            chosen = derive(git, head)
            # main moves to a commit with another reachable stable release before the request
            # is frozen; the submission must not see it.
            self.repo.tag("v0.13.0", self.repo.commit("advanced main", {"README": "moved"}))
            return chosen

        with mock.patch.object(gitrefs, "nightly_baseline", side_effect=derive_then_advance) as seen:
            code, out, _ = self.cli("--work", "telemetry", "--tag", self.TAG, "--head", "main")
        self.assertEqual(code, 0)
        self.assertEqual(seen.call_args.args[1], self.head)
        self.assertNotEqual(self.repo.git("rev-parse", "main"), self.head)
        request = json.loads(out)
        self.assertEqual((request["sources"]["baseline_sha"], request["sources"]["candidate_sha"]),
                         (self.base, self.head))
        self.assertEqual(request["submission"]["baseline"], {"tag": "v0.12.0", "sha": self.base})

    def test_nightly_tier_admits_five_bounded_build_steps_and_four_serial_15_minute_shards_within_180_minutes(self):
        """ADR-0266's budget on a plan frozen from the suite's nightly tier, through the real
        local path on a stepped clock: the build starts the run's deadline (ADR-0170), its five
        bounded steps (the supervisor, each side's af and probe) each take their declared
        build_minutes, and each serial shard passes the shard runner's own admission with its
        whole sampling window before that deadline. A fifth shard's window does not fit, and
        the deadline then refuses it."""
        self.release_history()
        self.c["core"].update(shards=4, cases=["cli_discovery__command_" + command for command in
                                               ("version", "help", "task_help", "review_run_help")])
        clock = [synthetic.RUN_STARTED_MS / 1000.0]
        wall = lambda: clock[0]
        preflight, reserve = 90, runner.FINISH_RESERVE_SECONDS
        windows = []
        start_execution = builds.Builder.start  # the real builder's, before its stand-in replaces it

        def build(builder):
            started = SimpleNamespace(wall=wall, plan=builder.plan, budget=SimpleNamespace(),
                                      deadline_unix_ms=builder.deadline_unix_ms)
            start_execution(started)
            # The stand-in's builds record holds the execution the real start fixes at this instant.
            self.assertEqual(started.execution["run"], synthetic.builds(builder.plan)["execution"]["run"])
            clock[0] += 5 * builder.plan["limits"]["build_minutes"] * 60

        def admit(run_dir, caller, index):
            """The shard runner's admission against the run's recorded deadline (a run bundle
            without a partition authority: the build's own); the remaining seconds it leaves."""
            raw = read(os.path.join(run_dir, "builds.json"))
            shard = SimpleNamespace(manifest=[contracts.read_json(raw, "builds"), raw], plan=plan,
                                    index=index, options=SimpleNamespace(
                                        wall=wall, clock=wall, deadline_unix_ms=caller,
                                        run_dir=run_dir))
            runner.Shard.check_run_deadline(shard)
            return shard.run_deadline - clock[0]

        def shard(options):
            left = admit(options.run_dir, options.deadline_unix_ms, options.shard)
            sampling = plan["limits"]["shard_sampling_minutes"] * 60
            self.assertLessEqual(preflight + sampling + reserve, left)
            windows.append(clock[0] + preflight)
            clock[0] += preflight + sampling + reserve

        with executor.Journal(self.c) as j:
            request = j.submit_nightly("core", self.TAG, self.head)
            plan = contracts.read_json(read(os.path.join(j.path(request["request_id"]), "plan.json")), "run")
            limits = plan["limits"]
            self.assertEqual((plan["tier"], len(plan["shards"]), limits["build_minutes"],
                              limits["shard_sampling_minutes"], limits["run_total_minutes"]),
                             ("nightly", 4, 20, 15, 180))
            self.assertNotEqual(plan["resolved"]["baseline_sha"], plan["resolved"]["candidate_sha"])
            calls = []
            path, _ = self.complete_core(j, request, calls, on_build=build, on_shard=shard)
        self.assertEqual(calls, ["build", "prepare", "shard_0", "shard_1", "shard_2", "shard_3", "cleanup"])
        start = synthetic.RUN_STARTED_MS / 1000.0
        # 100 build minutes, then four shards of 90 s preflight, 15 sampling minutes and the
        # finish reserve: 168 of 180 minutes, the last window closing 12.5 minutes early.
        self.assertEqual([(w - start) / 60 for w in windows], [101.5, 118.5, 135.5, 152.5])
        self.assertEqual((clock[0] - start) / 60, 168)
        left = admit(os.path.join(path, "run"), None, 3)
        self.assertEqual(left, 12 * 60)
        self.assertGreater(preflight + limits["shard_sampling_minutes"] * 60 + reserve, left)
        clock[0] += left
        with self.assertRaises(runner.Unavailable) as refused:
            admit(os.path.join(path, "run"), None, 3)
        self.assertEqual(refused.exception.reason, "run_deadline")


class ToolImage(TempDir):
    def test_musl_stage_holds_both_shipping_tools_in_the_executor_tool_directory(self):
        text = read(os.path.join(LAB, "images/Dockerfile")).decode()
        musl = text[text.index("FROM gnu AS musl"):]
        tools = "/opt/af-perf-tools/bin"
        self.assertIn('ln -s "$(command -v cargo-zigbuild)" %s/cargo-zigbuild' % tools, musl)
        self.assertIn("ln -s /opt/zig/zig %s/zig" % tools, musl)
        self.assertIn('test "$(%s/zig version)" = 0.15.2' % tools, musl)
        self.assertIn("%s/cargo-zigbuild --version | grep '0.23.4'" % tools, musl)
        # The earlier locations stay: cargo's own bin directory and /usr/local/bin/zig.
        self.assertIn("cargo install --locked cargo-zigbuild --version 0.23.4", musl)
        self.assertIn("ln -s /opt/zig/zig /usr/local/bin/zig", musl)
        example = read(os.path.join(LAB, "service/executor.example.toml")).decode()
        self.assertIn('# zigbuild_bin = "%s"' % tools, example)


PROCESS = r'''
import json, os, sys
sys.path.insert(0, sys.argv[1])
from perflab import hostlock, runner, telemetrylab, diagnostics, executor
from unittest import mock
mode, locks = sys.argv[2:4]
if mode == "hold":
    with hostlock.MachineLock(locks) as lease:
        lease.guard()
        print("started", flush=True)
        sys.stdin.readline()
        print("cleaning", flush=True)
        sys.stdin.readline()
        lease.confirm_clean()
    print("settled", flush=True)
elif mode == "orphan":
    from perflab import telemetryexec
    import time
    class Observed(telemetryexec.TrustedLocal):
        def launch(self, execution, stdout, stderr):
            process = super().launch(execution, stdout, stderr)
            print("started " + str(process.pid), flush=True)
            return process
    with hostlock.MachineLock(locks) as lease:
        lease.guard()
        work = sys.argv[4]
        os.mkdir(work)
        now = time.monotonic()
        execution = telemetryexec.Execution([sys.executable, "-I", "-B", "-c", "import time; time.sleep(120)"],
            {"PATH": "/usr/bin:/bin", "HOME": work, "TMPDIR": work}, work, [work], [], work,
            "orphan fixture", 65536, now + 125, 65536, cleanup_deadline=now + 128)
        Observed(sys.executable).run(execution)
        lease.confirm_clean()
elif mode == "core":
    try:
        runner.run_shard(runner.Options(lock_directory=locks))
    except runner.Unavailable as e:
        print(e.reason)
elif mode == "telemetry":
    with mock.patch.object(telemetrylab, "_binary", return_value=("/not-executed", "sha256:"+"1"*64)):
        try:
            telemetrylab.run(telemetrylab.Settings(profile="manual", trigger="manual", mode="trusted_local",
                baseline_af="x", candidate_af="y", baseline_sha="1"*40, candidate_sha="2"*40,
                authority_sha="3"*40, output=sys.argv[4], lock_directory=locks))
        except telemetrylab.Unavailable as e:
            print(e.reason)
elif mode == "profile":
    # Exercise the actual shared entry lock before _run_locked can launch any tool.
    from tests.test_executor import profile_contender
    profile_contender(locks, sys.argv[4])
elif mode == "scratch":
    import cli
    from types import SimpleNamespace
    original_lock = hostlock.MachineLock
    with mock.patch.object(hostlock, "MachineLock", side_effect=lambda *a: original_lock(locks)), \
            mock.patch.object(cli.hostprep, "prepare", side_effect=AssertionError("second preparation")), \
            mock.patch.object(cli, "_refuse", side_effect=lambda status, reason, detail: print(reason)):
        cli.cmd_ci_scratch(SimpleNamespace(cleanup=False, telemetry_owner=None,
            image=sys.argv[4] + ".img", mount=sys.argv[4], bytes=1024))
else:
    config = json.load(open(sys.argv[4]))
    with executor.Journal(config) as j:
        print(j.cycle(lock_directory=locks, execute=lambda *a: (_ for _ in ()).throw(AssertionError("second launch"))))
'''


def profile_contender(locks, output):
    from perflab import diagnostics
    from tests.support import lab_repo, read
    repo, sha = lab_repo(output + "-repo")
    sha = repo.commit("diagnostics", {diagnostics.REGISTRY_PATH: read(os.path.join(ROOT, diagnostics.REGISTRY_PATH))})
    plan = planning.plan(gitrefs.Git(repo.path), planning.Request(scope="partial_scope", trigger="manual", tier="quick",
        base=sha, head=sha, cases=["canonical_serialize_small"], build_image=IMAGE))
    path = output + ".json"
    with open(path, "wb") as f:
        f.write(contracts.write_bytes(plan, "run"))
    try:
        diagnostics.run_profile(gitrefs.Git(repo.path), diagnostics.Options(plan=path, cases=[plan["cases"][0]["instance_id"]],
            recipe="linux_gnu_trace", mode="trusted_local", output=output, lock_directory=locks))
    except diagnostics.Unavailable as e:
        print(e.reason)


class WorkloadAdmission(ExecutorFixture):
    """The selected releases' declared workloads, read from their retained sources under the
    installed authority, before any claim or build (the evaluator's `0.11.9` that the
    telemetry bench silently measured under the current fixture contract)."""

    def release(self, base, candidate):
        """A source pair whose Cargo.locks declare `base` and `candidate`."""
        lock = 'version = 4\n\n[[package]]\nname = "af"\nversion = "%s"\n'
        self.base = self.repo.commit("baseline " + base, {"Cargo.lock": lock % base})
        self.head = self.repo.commit("candidate " + candidate, {"Cargo.lock": lock % candidate})

    def test_unsupported_selected_releases_refuse_before_any_claim_or_build(self):
        self.c["core"]["cases"] = ["cli_discovery__command_version"]
        self.release("0.13.0", "0.11.9")
        for work, why in (("telemetry", "the candidate source %s is af 0.11.9: af 0.11.9 is "
                                        "named by no fixture contract"),
                          ("core", "the candidate source is af 0.11.9: no declared CLI "
                                   "adapter covers AF 0.11.9")):
            with self.subTest(work=work), executor.Journal(self.c) as j:
                why = why % self.head[:12] if "%s" in why else why
                refused = j.preflight(work, self.base, self.head, "old_" + work)
                contracts.write_bytes(refused, "executor-preflight")
                self.assertEqual(refused["status"], "refused")
                self.assertIn("refused before any claim or build", refused["detail"])
                self.assertIn(why, refused["detail"])
                with self.assertRaisesRegex(ContractError, "refused before any claim or build"):
                    self.submit(j, work, "old_" + work)
                self.assertEqual(j.names(), [], "a refusal reserves nothing")

    def test_the_supported_release_pair_is_admitted_with_each_sides_contract(self):
        self.c["core"]["cases"] = ["cli_discovery__command_version"]
        self.release("0.13.0", "0.14.0-rc.1")
        with executor.Journal(self.c) as j:
            for work, named in (("telemetry", "baseline af 0.13.0 under check-receipt@1, "
                                              "candidate af 0.14.0-rc.1 under check-receipt@2"),
                                ("core", "baseline af 0.13.0 with af_cli_v2, candidate af "
                                         "0.14.0-rc.1 with af_cli_v3")):
                ready = j.preflight(work, self.base, self.head, "pair_" + work)
                self.assertEqual(ready["status"], "ready", ready["detail"])
                # The admission's figures, as a comparison's member check records them.
                checked = {"serial": None, "tools": []}
                j._submit(work, self.base, self.head, "pair_" + work, checked=checked)
                self.assertIn(named, checked["admission"])
            self.assertEqual(j.names(), [])
            self.submit(j, "telemetry", "pair_telemetry")
            self.assertEqual(len(j.names()), 1)

    def assert_refused(self, work, key, why):
        """`preflight`, then `submit`, refuse with `why` and reserve, build or prepare nothing."""
        with mock.patch.object(executor.builds, "Builder", side_effect=AssertionError("built")), \
                mock.patch.object(executor.hostprep, "prepare",
                                  side_effect=AssertionError("prepared")), \
                executor.Journal(self.c) as j:
            refused = j.preflight(work, self.base, self.head, key)
            contracts.write_bytes(refused, "executor-preflight")
            self.assertEqual(refused["status"], "refused")
            self.assertIn("refused before any claim or build", refused["detail"])
            self.assertIn(why, refused["detail"])
            with self.assertRaisesRegex(ContractError, "refused before any claim or build"):
                self.submit(j, work, key)
            self.assertEqual(j.names(), [], "a refusal reserves nothing")

    def admitted(self, key):
        """The admission detail a ready core preflight checked; nothing is reserved."""
        with executor.Journal(self.c) as j:
            ready = j.preflight("core", self.base, self.head, key)
            self.assertEqual(ready["status"], "ready", ready["detail"])
            checked = {"serial": None, "tools": []}
            j._submit("core", self.base, self.head, key, checked=checked)
            self.assertEqual(j.names(), [])
            return checked["admission"]

    def test_unknown_or_absent_library_coverage_refuses_before_any_claim_or_build(self):
        # Library cases only: no CLI adapter is consulted, and a compile is never the proof.
        self.c["core"]["cases"] = ["canonical_serialize_small"]
        self.release("0.13.0", "0.11.9")
        self.assert_refused("core", "old_library", "the candidate source is af 0.11.9: no "
                            "declared native probe adapter covers AF 0.11.9")
        # An authority that declares no adapter registry has no library coverage at all.
        os.unlink(os.path.join(self.repo.path, "fixtures/performance/adapters.toml"))
        self.c["control_sha"] = self.repo.commit("authority without adapters")
        self.release("0.13.0", "0.14.0-rc.1")
        self.assert_refused("core", "no_registry", "has no fixtures/performance/adapters.toml; "
                            "its library workload has no declared coverage")

    def test_declared_library_coverage_is_admitted_for_each_side(self):
        self.c["core"]["cases"] = ["canonical_serialize_small", "state_projection"]
        for base, head in (("0.13.0", "0.14.0-rc.1"), ("0.12.0-rc.3", "0.12.0-rc.3")):
            with self.subTest(base=base, head=head):
                self.release(base, head)
                admission = self.admitted("library_" + head)
                self.assertIn("native workload: baseline af %s with native_probe_public_api_v1, "
                              "candidate af %s with native_probe_public_api_v1" % (base, head),
                              admission)

    def test_a_pending_cli_command_refuses_before_any_claim_or_build(self):
        # af_cli_v1 covers 0.12.0-rc.3 but declares both browser fixtures pending.
        self.c["core"]["cases"] = ["tui_open__registry_empty"]
        self.release("0.13.0", "0.12.0-rc.3")
        self.assert_refused("core", "pending_tui", "the candidate source is af 0.12.0-rc.3, "
                            "whose CLI adapter af_cli_v1 has no runnable command for "
                            "tui_open__registry_empty: The older command surface has no "
                            "reviewed browser registry setup.")

    def test_runnable_cli_commands_of_each_adapter_stay_admitted(self):
        # The same older adapter's discovery commands, and the browser cases of 0.13/0.14.
        for cases, base, head, named in (
                (["cli_discovery__command_version"], "0.12.0-rc.3", "0.12.0-rc.3",
                 "baseline af 0.12.0-rc.3 with af_cli_v1, candidate af 0.12.0-rc.3 with af_cli_v1"),
                (["tui_open__registry_empty", "tui_open__registry_stores_20x10000"], "0.13.0",
                 "0.14.0-rc.1", "baseline af 0.13.0 with af_cli_v2, candidate af 0.14.0-rc.1 "
                 "with af_cli_v3")):
            with self.subTest(cases=cases):
                self.c["core"]["cases"] = cases
                self.release(base, head)
                self.assertIn("CLI workload: " + named, self.admitted("cli_" + head))

    def test_a_cli_command_the_authority_does_not_declare_refuses_before_any_claim(self):
        # An authority whose older adapter omits the browser command has no CLI workload for
        # it: its registry is refused, never read as runnable.
        path = os.path.join(self.repo.path, "fixtures/performance/cli.toml")
        with open(path) as stream:
            text = stream.read()
        start = text.index("[[adapters.af_cli_v1.commands]]\ncase_id = \"tui_open\"")
        end = text.index("[adapters.af_cli_v2]")
        with open(path, "w") as stream:
            stream.write(text[:start] + text[end:])
        self.c["control_sha"] = self.repo.commit("authority without the older browser commands")
        self.c["core"]["cases"] = ["tui_open__registry_empty"]
        self.release("0.13.0", "0.12.0-rc.3")
        self.assert_refused("core", "missing_tui", "adapters.af_cli_v1: no command for "
                            "tui_open__registry_empty")


class ImageAdmission(ExecutorFixture):
    """The configured build and sample images and the shipping recipe's image-local tools,
    through a stand-in docker client, at preflight, submission and the claim (the Claw musl
    original that went uncertain 1.7 s after a `ready` preflight, its build image absent)."""

    SAMPLE = "registry.invalid/sample-only@sha256:" + "2" * 64

    def setUp(self):
        super().setUp()
        bin_dir = build_fixtures.fake_toolchain(self.path("image", "bin"),
                                                "x86_64-unknown-linux-musl")
        self.env = ["PATH=%s:/usr/bin:/bin" % bin_dir]
        self.zig = build_fixtures.fake_zigbuild(self.path("image-tools"))
        self.c.update(sample_image=self.SAMPLE, zigbuild_bin=self.zig)
        self.c["core"].update(tier="nightly", environment="linux_x86_64_musl_claw",
                              cases=["cli_discovery__command_version"])
        self.docker_dirs = 0
        for patcher in (mock.patch.object(executor.Journal, "images", REAL_JOURNAL_IMAGES),
                        mock.patch.object(executor.Journal, "lock_directory", self.locks)):
            patcher.start()
            self.addCleanup(patcher.stop)

    def docker(self, *present, lingering=None, volumes=None, identity=None, sample_probe=None):
        """Route every image admission to a stand-in docker client holding `present`."""
        self.docker_dirs += 1
        directory = self.path("docker-%d" % self.docker_dirs)
        images = {reference: ("sha256:" + "%d" % index * 64, self.env, [reference])
                  for index, reference in enumerate(present, 3)}
        docker = build_fixtures.fake_docker(directory, images, lingering, volumes, identity,
                                            sample_probe)
        patcher = mock.patch.object(executor, "image_admission", lambda config, recipes:
                                    REAL_IMAGE_ADMISSION(config, recipes, docker=docker))
        patcher.start()
        self.addCleanup(patcher.stop)
        return directory

    def fence(self):
        return os.path.lexists(os.path.join(self.locks, "af-perf-machine.lock.fence"))

    def test_missing_images_and_shipping_tools_refuse_before_any_claim_or_build(self):
        cases = ((("sample",), "the build image %s is not present" % IMAGE, {}),
                 (("build",), "the sample image %s is not present" % self.SAMPLE, {}),
                 (("build", "sample"), "the recipe pins cargo-zigbuild 0.23.4",
                  {"zigbuild_bin": self.path("no-image-tools")}),
                 (("build", "sample"), "the recipe pins Zig 0.15.2",
                  {"zigbuild_bin": build_fixtures.fake_zigbuild(self.path("old-zig"),
                                                                zig="0.15.1")}))
        for index, (present, why, change) in enumerate(cases):
            with self.subTest(why=why):
                directory = self.docker(*[{"build": IMAGE, "sample": self.SAMPLE}[name]
                                          for name in present])
                c = dict(self.c, **change)
                with executor.Journal(c) as j:
                    refused = j.preflight("core", self.base, self.head, "images_%d" % index)
                    contracts.write_bytes(refused, "executor-preflight")
                    self.assertEqual(refused["status"], "refused")
                    self.assertIn("refused before any claim or build", refused["detail"])
                    self.assertIn(why, refused["detail"])
                    self.assertEqual(refused["tools"][-1]["state"], "unavailable")
                    with self.assertRaisesRegex(ContractError, why):
                        self.submit(j, "core", "images_%d" % index)
                    self.assertEqual(j.names(), [], "a refusal reserves nothing")
                runs = [call for call in build_fixtures.docker_calls(directory)
                        if call[0] == "run"]
                self.assertTrue(all(call[call.index("--name") + 1].startswith("af-perf-probe-")
                                    for call in runs), "only bounded probe containers ran")
                self.assertFalse(self.fence())

    def test_an_unusable_sample_runtime_tool_or_containment_refuses_before_any_claim(self):
        # The sample image's actual probe container, not its metadata: a missing interpreter,
        # shell or driver module, or a container without the samples' containment, refuses
        # the submission before anything is reserved, claimed or built, and the probe's own
        # container is removed (the fence settles clean).
        usable = {"uid": os.getuid(), "gid": os.getgid(), "cap_eff": "0000000000000000",
                  "no_new_privs": "1", "networks": ["lo"], "cgroup2": True,
                  "root_writable": False, "modules": True, "shell": True, "python": "3.12.3"}
        cases = (((127, ""), "cannot run /usr/bin/python3: exit 127"),
                 ((0, "not a record"), "answered no probe record"),
                 ((0, json.dumps(dict(usable, shell=False))), "lacks /bin/sh"),
                 ((0, json.dumps(dict(usable, modules=False))), "lacks the driver modules"),
                 ((0, json.dumps(dict(usable, networks=["eth0", "lo"]))), "the networks"),
                 ((0, json.dumps(dict(usable, cap_eff="00000000a80425fb"))),
                  "effective capabilities"),
                 ((0, json.dumps(dict(usable, no_new_privs="0"))), "no_new_privs"),
                 ((0, json.dumps(dict(usable, cgroup2=False))), "no cgroup v2"),
                 ((0, json.dumps(dict(usable, root_writable=True))), "root filesystem is "
                  "writable"),
                 ((0, json.dumps(dict(usable, uid=0))), "runs as 0:"))
        for index, (answer, why) in enumerate(cases):
            with self.subTest(why=why):
                directory = self.docker(IMAGE, self.SAMPLE, sample_probe=answer)
                with executor.Journal(self.c) as j:
                    refused = j.preflight("core", self.base, self.head, "sample_%d" % index)
                    contracts.write_bytes(refused, "executor-preflight")
                    self.assertEqual(refused["status"], "refused")
                    self.assertIn("refused before any claim or build", refused["detail"])
                    self.assertIn("the sample image %s" % self.SAMPLE, refused["detail"])
                    self.assertIn(why, refused["detail"])
                    with self.assertRaisesRegex(ContractError, why):
                        self.submit(j, "core", "sample_%d" % index)
                    self.assertEqual(j.names(), [], "a refusal reserves nothing")
                runs = [call for call in build_fixtures.docker_calls(directory)
                        if call[0] == "run"]
                probes = [call for call in runs if "AF_PERF_SAMPLE_PROBE=1" in call]
                self.assertTrue(probes, "the sample image ran its probe container")
                self.assertTrue(all(call[call.index("--entrypoint") + 2] == self.SAMPLE
                                    and "--read-only" in call and "none" in call
                                    for call in probes))
                self.assertTrue(all(call[call.index("--name") + 1].startswith("af-perf-probe-")
                                    for call in runs), "only bounded probe containers ran")
                self.assertFalse(self.fence())

    def test_ready_admission_is_rechecked_by_the_claim_before_any_build(self):
        self.docker(IMAGE, self.SAMPLE)
        with executor.Journal(self.c) as j:
            ready = j.preflight("core", self.base, self.head, "rechecked")
            self.assertEqual(ready["status"], "ready", ready["detail"])
            tools = {item["name"]: item for item in ready["tools"]}
            self.assertIn(IMAGE, tools["build_image"]["detail"])
            self.assertIn(self.SAMPLE, tools["sample_image"]["detail"])
            self.assertIn("rustc 1.88.0", tools["image_toolchain"]["detail"])
            self.assertIn("cargo-zigbuild 0.23.4; zig 0.15.2", tools["image_toolchain"]["detail"])
            self.assertIn("the sample image %s Python 3.12.3" % self.SAMPLE,
                          tools["sample_runtime"]["detail"])
            self.assertIn("no network, no capability, no new privileges, cgroup v2 and a "
                          "read-only root", tools["sample_runtime"]["detail"])
            self.assertFalse(self.fence(), "the probes' exclusion settled clean")
            request = self.submit(j, "core", "rechecked")
            # The build image disappears after submission: the claim's own admission ends the
            # original failed before any build, nothing measured, and keeps no fence.
            self.docker(self.SAMPLE)
            with mock.patch.object(builds, "Builder",
                                   side_effect=AssertionError("built before admission")):
                self.assertEqual(j.cycle(lock_directory=self.locks), "failed")
            state = j.state(request["request_id"])
            self.assertIn("core refused before any build, nothing measured", state["detail"])
            self.assertIn("the build image %s is not present" % IMAGE, state["detail"])
            result = contracts.read_json(read(os.path.join(j.path(request["request_id"]),
                                                           "result.json")), "executor-result")
            self.assertEqual(result["measurement_status"], "unavailable")
            self.assertFalse(self.fence())

    def test_an_unconfirmed_probe_container_keeps_the_fence(self):
        self.docker(IMAGE, self.SAMPLE, lingering="af-perf-probe-")
        with executor.Journal(self.c) as j:
            refused = j.preflight("core", self.base, self.head, "lingering")
            self.assertEqual(refused["status"], "refused")
            self.assertIn("build_container_unconfirmed", refused["detail"])
            self.assertEqual(j.names(), [])
        self.assertTrue(self.fence(), "an unconfirmed removal is never settled")


    def test_an_image_volume_refusal_runs_nothing_and_keeps_no_fence(self):
        # Only the read-only `docker image inspect` runs before the policy refuses: no probe
        # container, no reservation and no fence; a malformed declaration refuses the same.
        for index, volumes in enumerate(('{"/unexpected-volume":{}}', "not json")):
            with self.subTest(volumes=volumes):
                directory = self.docker(IMAGE, self.SAMPLE, volumes=volumes)
                with executor.Journal(self.c) as j:
                    refused = j.preflight("core", self.base, self.head, "volumes_%d" % index)
                    contracts.write_bytes(refused, "executor-preflight")
                    self.assertEqual(refused["status"], "refused")
                    self.assertIn("refused before any claim or build", refused["detail"])
                    self.assertIn("build_image_refused", refused["detail"])
                    with self.assertRaisesRegex(ContractError, "image volume declaration|"
                                                "declares implicit writable volumes"):
                        self.submit(j, "core", "volumes_%d" % index)
                    self.assertEqual(j.names(), [], "a refusal reserves nothing")
                calls = build_fixtures.docker_calls(directory)
                self.assertTrue(calls)
                self.assertTrue(all(call[:2] == ["image", "inspect"] for call in calls), calls)
                self.assertFalse(self.fence(), "a read-only refusal is settled clean")

    def test_a_malformed_image_identity_refuses_typed_and_keeps_no_fence(self):
        # A successful `docker image inspect` whose five identity fields are not JSON, or not
        # of their shape, is a typed refusal: nothing claimed, built, probed or fenced.
        good = ['"sha256:%s"' % ("3" * 64), '"linux"', '"arm64"', '["%s"]' % IMAGE, '[]']
        # Every field and shape is covered by `test_builds.ImageIdentity`; here, one five-field
        # output with every field not JSON, one with a misshapen environment, one nested 1200
        # deep below the byte bound and one whose environment PATH holds the raw byte 0xff.
        bad = (("\t".join(["{"] * 5) + "\n", "is not JSON"),
               ("\t".join(good[:4] + ["[1]"]) + "\n", "is not a list of strings"),
               ("\t".join(good[:4] + ["[" * 1200 + "]" * 1200]) + "\n", "nests deeper"),
               ("\t".join(good[:4]).encode() + b'\t["PATH=/usr/\xffbin"]\n', "not UTF-8"))
        for index, (identity, why) in enumerate(bad):
            with self.subTest(why=why):
                directory = self.docker(IMAGE, self.SAMPLE, identity=identity)
                with executor.Journal(self.c) as j:
                    refused = j.preflight("core", self.base, self.head, "identity_%d" % index)
                    contracts.write_bytes(refused, "executor-preflight")
                    self.assertEqual(refused["status"], "refused")
                    self.assertIn("refused before any claim or build", refused["detail"])
                    self.assertIn("build_image_malformed: docker image inspect gave a "
                                  "malformed identity for the build image", refused["detail"])
                    self.assertIn(why, refused["detail"])
                    with self.assertRaisesRegex(ContractError, "build_image_malformed"):
                        self.submit(j, "core", "identity_%d" % index)
                    self.assertEqual(j.names(), [], "a refusal reserves nothing")
                calls = build_fixtures.docker_calls(directory)
                self.assertTrue(calls)
                self.assertTrue(all(call[:2] == ["image", "inspect"] for call in calls), calls)
                self.assertFalse(self.fence(), "a read-only refusal is settled clean")

    def test_a_claimed_image_volume_refusal_fails_before_any_build_without_a_fence(self):
        self.docker(IMAGE, self.SAMPLE)
        with executor.Journal(self.c) as j:
            request = self.submit(j, "core", "claimed_volumes")
            # The build image declares a volume after submission: the claim's own admission
            # ends the original failed before any build, nothing measured, and no fence.
            directory = self.docker(IMAGE, self.SAMPLE, volumes='{"/unexpected-volume":{}}')
            with mock.patch.object(builds, "Builder",
                                   side_effect=AssertionError("built before admission")):
                self.assertEqual(j.cycle(lock_directory=self.locks), "failed")
            state = j.state(request["request_id"])
            self.assertIn("core refused before any build, nothing measured", state["detail"])
            self.assertIn("declares implicit writable volumes", state["detail"])
        self.assertTrue(all(call[:2] == ["image", "inspect"]
                            for call in build_fixtures.docker_calls(directory)))
        self.assertFalse(self.fence())


class ScratchCommand(TempDir):
    def test_preparation_and_cleanup_settle_only_confirmed_terminal_work(self):
        import cli
        locks = self.path("machine")
        os.mkdir(locks)
        original_lock = hostlock.MachineLock
        args = SimpleNamespace(cleanup=False, telemetry_owner=None, image=self.path("image"),
                               mount=self.path("mount"), bytes=1024)
        with mock.patch.object(hostlock, "MachineLock", side_effect=lambda *a: original_lock(locks)), \
                mock.patch.object(cli.hostprep, "prepare", return_value={"size_bytes": 1024,
                    "mount": args.mount, "free_bytes": 2048}) as prepare, \
                mock.patch.object(cli.hostprep, "cleanup", return_value=[]) as cleanup:
            self.assertEqual(cli.cmd_ci_scratch(args), 0)
            prepare.assert_called_once()
            with original_lock(locks):
                pass
            args.cleanup = True
            self.assertEqual(cli.cmd_ci_scratch(args), 0)
            cleanup.assert_called_once()
            with original_lock(locks):
                pass
            cleanup.return_value = ["unmount unconfirmed"]
            self.assertNotEqual(cli.cmd_ci_scratch(args), 0)
        with self.assertRaisesRegex(hostlock.Refused, "unfinished experiment"):
            original_lock(locks)


class Processes(ExecutorFixture):
    def launch(self, mode, suffix, extra=""):
        private = self.path(suffix)
        os.makedirs(private)
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": private, "TMPDIR": private,
               "AF_PERF_LOCK_FD": "0", "AF_PERF_LOCK_HELD": "1"}  # no bypass authority
        return subprocess.Popen([sys.executable, "-I", "-B", "-c", PROCESS, LAB, mode, self.locks, extra],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=private, text=True)

    def test_all_entrypoints_contend_across_processes_until_cleanup_is_finished(self):
        with executor.Journal(self.c) as j:
            self.submit(j)
        config_path = self.path("config.json")
        with open(config_path, "w") as f:
            json.dump(self.c, f)
        holder = self.launch("hold", "holder")
        def settle():
            if holder.poll() is None:
                holder.kill()
            holder.communicate(timeout=30)
        self.addCleanup(settle)
        self.assertEqual(holder.stdout.readline().strip(), "started")
        for phase in ("running", "cleanup"):
            if phase == "cleanup":
                holder.stdin.write("\n")
                holder.stdin.flush()
                self.assertEqual(holder.stdout.readline().strip(), "cleaning")
            for mode in ("core", "telemetry", "profile", "scratch", "service"):
                child = self.launch(mode, mode + phase, config_path if mode == "service" else self.path(mode + phase + "-out"))
                out, err = child.communicate(timeout=30)
                self.assertEqual(child.returncode, 0, err)
                self.assertEqual(out.strip(), "busy" if mode == "service" else "concurrent_shard", err)
        out, err = holder.communicate("\n", timeout=30)
        self.assertEqual(out.strip(), "settled", err)
        self.assertEqual(holder.returncode, 0, err)
        with hostlock.MachineLock(self.locks):
            pass

    def test_orphaned_owned_child_cannot_release_exclusion_into_live_resources(self):
        controller = self.launch("orphan", "orphan-controller", self.path("owned-child-work"))
        line = controller.stdout.readline().strip()
        self.assertTrue(line.startswith("started "), line)
        pid = int(line.split()[1])
        try:
            controller.kill()
            controller.communicate(timeout=30)
            os.kill(pid, 0)  # It is this fixture's still-live, separately owned child.
            with self.assertRaisesRegex(hostlock.Refused, "unfinished experiment"):
                hostlock.MachineLock(self.locks)
        finally:
            if controller.poll() is None:
                controller.kill()
                controller.communicate(timeout=30)
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            until = time.monotonic() + 5
            def executing():
                state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=2,
                    env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"}).stdout.strip()
                return bool(state) and not state.startswith("Z")
            while executing() and time.monotonic() < until:
                time.sleep(.01)
            self.assertFalse(executing())
        # Cleanup of the child does not itself erase the earlier uncertainty fence.
        with self.assertRaises(hostlock.Refused):
            hostlock.MachineLock(self.locks)

    def test_lost_controller_retains_fence_and_rejects_fd_and_path_bypasses(self):
        holder = self.launch("hold", "holder")
        self.assertEqual(holder.stdout.readline().strip(), "started")
        holder.kill()
        holder.communicate(timeout=30)
        with self.assertRaisesRegex(hostlock.Refused, "unfinished experiment"):
            hostlock.MachineLock(self.locks)
        with self.assertRaises(hostlock.Refused):
            hostlock.acquire(lease=0)
        with self.assertRaises(hostlock.Refused):
            hostlock.acquire(lease=SimpleNamespace(fd=0))


class AuthorityAndShapes(TempDir):
    def test_only_explicit_operator_mirror_gets_cross_account_git_trust(self):
        repo, sha = lab_repo(self.path("source-mirror"))
        config_doc = config(self.path("journal"), repo.path, sha)
        actual_run = subprocess.run
        def different_owner(argv, **kwargs):
            kwargs["env"] = dict(kwargs["env"], GIT_TEST_ASSUME_DIFFERENT_OWNER="1")
            return actual_run(argv, **kwargs)
        with mock.patch.object(gitrefs.subprocess, "run", side_effect=different_owner):
            with self.assertRaises(gitrefs.Unavailable):
                gitrefs.Git(repo.path).resolve(sha, "untrusted mirror")
            trusted = executor.source_repository(config_doc)
            self.assertEqual(trusted.resolve(sha, "operator mirror"), sha)
            self.assertEqual(trusted.exact_ref("refs/heads/main", "reviewed main"), sha)
        alias = self.path("alias")
        os.symlink(repo.path, alias)
        config_doc["repository_path"] = alias
        with self.assertRaisesRegex(ContractError, "links or aliases"):
            executor.source_repository(config_doc)

    def test_backend_environment_and_private_paths_are_validated_without_fallback(self):
        c = config(self.path("state"), self.tmp, "1" * 40)
        original = executor.environment(c)
        c["backend"] = "github_actions"
        self.assertNotEqual(executor.environment(c), original)
        c["backend"] = "claw_runner_label"
        with self.assertRaises(ContractError):
            contracts.validate_executor(c)
        os.symlink(self.tmp, self.path("alias"))
        with self.assertRaises(ContractError):
            executor.private_directory(self.path("alias"))
        for kind in ("link", "hardlink", "directory", "writable"):
            directory = self.path(kind)
            os.mkdir(directory)
            path = os.path.join(directory, hostlock.MACHINE_LOCK_NAME)
            if kind == "link":
                os.symlink(self.path("missing"), path)
            elif kind == "hardlink":
                open(self.path("target"), "w").close()
                os.link(self.path("target"), path)
            elif kind == "directory":
                os.mkdir(path)
            else:
                open(path, "w").close()
                os.chmod(path, 0o666)
            with self.subTest(kind=kind), self.assertRaises(hostlock.Refused):
                hostlock.MachineLock(directory)

    def test_shipping_tool_directory_is_explicit_canonical_and_changes_environment(self):
        c = config(self.path("journal"), self.tmp, "1" * 40)
        self.assertIsNone(c["zigbuild_bin"])  # GNU starts without shipping tools.
        gnu = executor.environment(c)
        c["zigbuild_bin"] = "/opt/pinned-tools/bin"
        contracts.validate_executor(c)
        self.assertNotEqual(executor.environment(c), gnu)
        c["core"].update(tier="nightly", environment="linux_x86_64_musl_claw")
        for value in (None, "tools/bin", "/opt/../tools", "/opt/tools/", "/opt/tools:/bin",
                      "/opt/tools\n", "/opt/$(command)", ""):
            with self.subTest(value=value), self.assertRaises(ContractError):
                contracts.validate_executor(dict(c, zigbuild_bin=value))
        del c["zigbuild_bin"]
        with self.assertRaisesRegex(ContractError, "requires zigbuild_bin"):
            contracts.validate_executor(c)
        obsolete = config(self.path("old"), self.tmp, "1" * 40)
        for schema in ("af.perf-executor/2", "af.perf-executor/3"):
            obsolete["schema"] = schema
            with self.assertRaisesRegex(ContractError, "expected.*af.perf-executor/4"):
                contracts.validate_executor(obsolete)

    def test_service_refuses_missing_or_invalid_shipping_configuration_before_reservation(self):
        import cli
        raw = read(os.path.join(LAB, "service/executor.example.toml")).decode()
        config_path = self.path("operator.toml")
        for tier in ("nightly", "release"):
            for setting in ("", 'zigbuild_bin = "tools/bin"\n'):
                with self.subTest(tier=tier, setting=setting):
                    text = raw.replace('tier = "quick"', 'tier = "' + tier + '"').replace(
                        'environment = "linux_x86_64_gnu_claw"', 'environment = "linux_x86_64_musl_claw"')
                    text = text.replace('[core]', setting + '\n[core]')
                    with open(config_path, "w") as stream:
                        stream.write(text)
                    with mock.patch.object(executor, "Journal") as journal, \
                            mock.patch.object(builds, "Builder") as builder:
                        self.assertEqual(cli.cmd_executor(SimpleNamespace(config=config_path,
                            operation="service", work="core")), 4)
                    journal.assert_not_called()
                    builder.assert_not_called()

    def test_lock_lease_refuses_inode_replacement_and_inherited_capabilities(self):
        with hostlock.MachineLock(self.tmp) as lease:
            old = lease.path + ".held"
            os.rename(lease.path, old)  # adversarial fixture, never production cleanup
            with open(lease.path, "w") as f:
                f.write("replaced")
            with self.assertRaisesRegex(hostlock.Refused, "inode changed"):
                lease.check()
        with self.assertRaises(hostlock.Refused):
            hostlock.acquire(lease=3)

    def test_machine_inode_anchor_refuses_private_tmp_and_never_creates_a_second_lock(self):
        host = self.path("host")
        private = self.path("private-tmp")
        os.mkdir(host)
        os.mkdir(private)
        with hostlock.MachineLock(host) as lease:
            info = os.stat(lease.path)
            identity = [info.st_dev, info.st_ino]
        with hostlock.MachineLock(host, expected_identity=identity):
            pass
        with self.assertRaises(hostlock.Refused):
            hostlock.MachineLock(private, expected_identity=identity)
        self.assertEqual(os.listdir(private), [])
        with mock.patch.object(hostlock, "MACHINE_LOCK_ANCHOR", self.path("absent-anchor")):
            with self.assertRaisesRegex(hostlock.Refused, "requires root-owned"):
                hostlock.read_anchor(required=True)

    def test_retained_shallow_sources_feed_the_existing_builder_without_remote_transport(self):
        repo, base = lab_repo(self.path("repo"))
        head = repo.commit("candidate", {"README": "head"})
        path = self.path("request")
        os.mkdir(path)
        request = {"sources": {"baseline_sha": base, "candidate_sha": head,
                               "control_sha": base, "authority_sha": base}}
        source = executor.retain_sources({"repository_path": repo.path}, request, path,
            budget.Budget(time.monotonic() + 30, time.monotonic, 0))
        home = self.path("git-home")
        os.mkdir(home)
        layout = SimpleNamespace(owner=source, git=self.path("build.git"), home=home)
        self.assertTrue(builds.private_repository(layout, [base, head]))
        for sha in (base, head):
            self.assertEqual(gitrefs.Git(layout.git).resolve(sha, "builder source"), sha)

    def test_exact_installed_producer_matches_and_candidate_python_is_never_imported(self):
        repo, _ = lab_repo(self.path("repo"))
        registry = telemetrylab.load_registry()[0]
        names = telemetrylab.authority_files(registry)
        files = {name: read(os.path.join(ROOT, name)) for name in names}
        sha = repo.commit("synthetic Git copy of installed authority", files)
        self.assertTrue(executor.producer(gitrefs.Git(repo.path), sha).startswith("sha256:"))
        sentinel = self.path("executed-candidate")
        changed = repo.commit("candidate Python data", {"scripts/performance/perflab/executor.py":
            "open(%r, 'w').write('must never execute')" % sentinel})
        with self.assertRaisesRegex(ContractError, "installed control bytes differ"):
            executor.producer(gitrefs.Git(repo.path), changed)
        self.assertFalse(os.path.exists(sentinel))

    def test_installed_producer_is_checked_against_git_and_refuses_candidate_python(self):
        repo, sha = lab_repo(self.path("repo"))
        with self.assertRaises(ContractError):
            executor.producer(gitrefs.Git(repo.path), sha)
