"""Independent executor: real journal/Git/locks/admission, injected bounded compute.

Synthetic samples and tool stand-ins exercise orchestration only, never Docker or speed.
"""
import copy
import json
import os
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
from perflab.values import ContractError, canonical_digest, sha256_bytes
from tests import telemetry_fixtures as tf
from tests.support import ROOT, TempDir, lab_repo, read

LAB = os.path.join(ROOT, "scripts/performance")
IMAGE = "registry.invalid/sample@sha256:" + "1" * 64
PRODUCER = synthetic.label_digest("installed-producer-test-only")


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

    def submit(self, journal, work="core", key="manual_1"):
        return journal.submit(work, self.base, self.head, key)


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

    def test_core_path_uses_retained_git_native_archive_and_reports_failed_verdict(self):
        """Real local orchestration; synthetic builder/shards and filesystem stand-ins."""
        with executor.Journal(self.c) as j:
            request = self.submit(j)
            path = j.path(request["request_id"])
            plan = contracts.read_json(read(os.path.join(path, "plan.json")), "run")
            staged = synthetic.write_run(plan, self.path("synthetic"),
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
            self.assertEqual(calls, ["build", "prepare", "shard_0", "cleanup"])
            original, state, result = executor.verify_original(j, request["request_id"])
            self.assertEqual(result["measurement_status"], "fail")
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

    def test_local_telemetry_builds_both_sides_verifies_raw_and_keeps_separate_history(self):
        self.c["core"]["cases"] = ["canonical_serialize_small"]
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
            self.assertTrue(all(i[1] == IMAGE and 30 < i[2] <= 4800 for i in seen))
            _, _, result = executor.verify_original(j, request["request_id"])
            self.assertEqual(result["measurement_status"], "complete")
            history_path = os.path.join(self.c["root"], "local-telemetry-history/history.json")
            hist = contracts.read_json(read(history_path), "telemetry-history")
            self.assertEqual(len(hist["results"]), 1)
            self.assertTrue(hist["results"][0]["result_id"].startswith("local_"))
            self.assertFalse(os.path.exists(os.path.join(self.c["root"], "hosted-telemetry-history")))


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
