"""Independent executor: real journal/Git/locks/admission, injected bounded compute.

Synthetic samples and tool stand-ins exercise orchestration only, never Docker or speed.
"""
import copy
import io
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
from perflab.values import ContractError, canonical_digest, document_bytes, sha256_bytes
from tests import build_fixtures
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
            # What the previous release wrote: request/1, result/2 without binaries.
            old_request = dict(request, schema="af.perf-executor-request/1")
            old_result = dict(result, schema="af.perf-executor-result/2")
            del old_result["binaries"]
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
        with executor.Journal(self.c) as j:
            request = self.submit(j, "profile", "uncertain-profile")
            path = j.path(request["request_id"])
            with mock.patch.object(builds, "Builder", Builder), \
                    mock.patch.object(executor.hostprep, "prepare", prepare), \
                    mock.patch.object(executor.hostprep, "cleanup") as cleanup, \
                    mock.patch.object(diagnostics, "run_profile", profile), \
                    mock.patch.object(diagnostics, "probe_tools") as tools:
                with self.assertRaises(diagnostics.Unavailable) as caught:
                    j.cycle(lock_directory=self.locks)
            self.assertEqual(caught.exception.reason, "build_container_unconfirmed")
            self.assertFalse(caught.exception.cleanup_confirmed)
            self.assertTrue(borrowed[0].uncertain)
            cleanup.assert_not_called()
            tools.assert_not_called()
            self.assertEqual(calls, ["build", "build"])
            self.assertEqual(j.state(request["request_id"])["state"], "uncertain")
            self.assertIn(b"af-perf-probe-", read(os.path.join(path, "profile/cleanup-uncertain.txt")))
            self.assertTrue(os.path.isdir(os.path.join(path, "scratch")))
            self.assertEqual(j.cycle(lock_directory=self.locks), "idle")
        with self.assertRaisesRegex(hostlock.Refused, "unfinished experiment"):
            hostlock.MachineLock(self.locks)

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
        self.assertEqual(request["schema"], "af.perf-executor-request/2")
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

    def test_nightly_tier_admits_two_builds_and_four_serial_30_minute_shards_within_180_minutes(self):
        """ADR-0266's budget on a plan frozen from the suite's nightly tier, through the real
        local path on a stepped clock: the build starts the run's deadline (ADR-0170), the
        baseline and candidate builds take their declared build_minutes, and each serial shard
        passes the shard runner's own admission with its whole sampling window before that
        deadline. A fifth shard's window does not fit, and the deadline then refuses it."""
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
            clock[0] += 2 * builder.plan["limits"]["build_minutes"] * 60

        def admit(run_dir, caller):
            """The shard runner's admission against the run's recorded deadline; the remaining
            seconds it leaves."""
            built = contracts.read_file(os.path.join(run_dir, "builds.json"), "builds")
            shard = SimpleNamespace(manifest=[built], plan=plan, options=SimpleNamespace(
                wall=wall, clock=wall, deadline_unix_ms=caller))
            runner.Shard.check_run_deadline(shard)
            return shard.run_deadline - clock[0]

        def shard(options):
            left = admit(options.run_dir, options.deadline_unix_ms)
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
                             ("nightly", 4, 20, 30, 180))
            self.assertNotEqual(plan["resolved"]["baseline_sha"], plan["resolved"]["candidate_sha"])
            calls = []
            path, _ = self.complete_core(j, request, calls, on_build=build, on_shard=shard)
        self.assertEqual(calls, ["build", "prepare", "shard_0", "shard_1", "shard_2", "shard_3", "cleanup"])
        start = synthetic.RUN_STARTED_MS / 1000.0
        # 40 build minutes, then four shards of 90 s preflight, 30 sampling minutes and the
        # finish reserve: 168 of 180 minutes, the last window closing 12.5 minutes early.
        self.assertEqual([(w - start) / 60 for w in windows], [41.5, 73.5, 105.5, 137.5])
        self.assertEqual((clock[0] - start) / 60, 168)
        left = admit(os.path.join(path, "run"), None)
        self.assertEqual(left, 12 * 60)
        self.assertGreater(preflight + limits["shard_sampling_minutes"] * 60 + reserve, left)
        clock[0] += left
        with self.assertRaises(runner.Unavailable) as refused:
            admit(os.path.join(path, "run"), None)
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
        obsolete["schema"] = "af.perf-executor/1"
        with self.assertRaisesRegex(ContractError, "expected.*af.perf-executor/2"):
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
