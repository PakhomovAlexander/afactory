#!/usr/bin/env python3
"""Hermetic CLI/background merge watcher tests. Real Git; read-only gh socket fixture."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import queue
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "scripts/scenario-merge-watch.py"
spec = importlib.util.spec_from_file_location("merge_watch", SOURCE)
W = importlib.util.module_from_spec(spec)
spec.loader.exec_module(W)

# The mock CLI enforces the complete read-only argv, not just the HTTP response body.
GH = '''#!/usr/bin/env python3
import json, os, socket, sys
assert sys.argv[1:7] == ["api", "--hostname", "github.com", "--method", "GET", sys.argv[6]]
assert len(sys.argv) == 7 and sys.argv[6].startswith("repos/octo/project/")
with socket.socket(socket.AF_UNIX) as s:
    s.connect(os.environ["WATCH_FIXTURE_SOCKET"])
    s.sendall((sys.argv[6] + "\\n").encode())
    s.shutdown(socket.SHUT_WR)
    raw = bytearray()
    while True:
        data = s.recv(65536)
        if not data:
            break
        raw.extend(data)
    response = json.loads(raw)
    if "error" in response:
        sys.exit(1)
    print(json.dumps(response))
'''


class Service:
    """Each read is a rendezvous: the test controls a later human merge without polling."""
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.bind(str(path))
        self.socket.listen(4)
        self.requests = queue.Queue()
        self.errors = []
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        try:
            while True:
                connection, _ = self.socket.accept()
                raw = bytearray()
                while data := connection.recv(4096):
                    raw.extend(data)
                if not raw:
                    connection.close()
                    return
                self.requests.put((raw.decode().strip(), connection))
        except OSError as exc:
            self.errors.append(exc)

    def take(self, endpoint):
        route, connection = self.requests.get(timeout=20)
        if route != "repos/octo/project" + endpoint:
            connection.close()
            raise AssertionError((route, endpoint))
        return connection

    @staticmethod
    def reply(connection, value):
        with connection:
            connection.sendall(json.dumps(value).encode())

    def close(self):
        with socket.socket(socket.AF_UNIX) as s:
            s.connect(self.socket.getsockname())
        self.thread.join(timeout=5)
        self.socket.close()
        while not self.requests.empty():
            _, connection = self.requests.get_nowait()
            connection.close()


class WatchTests(unittest.TestCase):
    def setUp(self):
        # A short /tmp socket path also fits macOS's Unix-domain address limit.
        self.temp = tempfile.TemporaryDirectory(prefix="merge-watch-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.home = self.root / "home"
        self.home.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        gh = self.bin / "gh"
        gh.write_text(GH)
        gh.chmod(0o700)
        self.env = {**os.environ, "HOME": str(self.home), "XDG_CONFIG_HOME": str(self.home),
                    "XDG_STATE_HOME": str(self.home), "XDG_CACHE_HOME": str(self.home),
                    "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "WATCH_FIXTURE_SOCKET": str(self.root / "gh.sock"),
                    "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
                    "GIT_AUTHOR_NAME": "Fixture", "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
                    "GIT_COMMITTER_NAME": "Fixture", "GIT_COMMITTER_EMAIL": "fixture@example.invalid"}
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        (self.repo / "file").write_text("base\n")
        (self.repo / "a").mkdir()
        (self.repo / "a/file").write_text("nested\n")
        (self.repo / "a.txt").write_text("tree ordering\n")
        (self.repo / "link").symlink_to("file")
        (self.repo / ".gitignore").write_text("target/\n")
        self.git("add", ".")
        self.git("commit", "-m", "base")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.tree = self.root / "task-worktree"
        self.git("worktree", "add", "-b", "fix", str(self.tree))
        (self.tree / "file").write_text("fixed\n")
        self.git("add", "file", repo=self.tree)
        self.git("commit", "-m", "fix", repo=self.tree)
        self.head = self.git("rev-parse", "HEAD", repo=self.tree).strip()
        self.head_tree = self.git("rev-parse", "HEAD^{tree}", repo=self.tree).strip()
        self.git("branch", "verification", self.base)
        self.store = self.root / "task-store"
        self.store.mkdir()
        (self.store / "events.sqlite").write_bytes(b"audit evidence")
        self.config = {"schema": W.CONFIG, "repository": "octo/project", "repository_id": 17,
                       "pr": 593, "head": self.head, "local_repo": str(self.repo),
                       "state_dir": str(self.root / "state"), "required_checks": ["CI"],
                       "worktrees": [{"path": str(self.tree), "branch": "fix", "head": self.head}],
                       "branches": [{"branch": "fix", "head": self.head},
                                    {"branch": "verification", "head": self.base}],
                       "poll_seconds": 1, "max_seconds": 60, "max_polls": 10, "max_errors": 2}
        self.config_path = self.root / "config.json"
        self.write_config()
        self.service = Service(self.root / "gh.sock")
        self.addCleanup(self.service.close)
        self.executable = self.root / "watch.py"
        # Preserve all production bytes and entry points. Only the wait clock is virtualized;
        # service rendezvous controls observation order, with no sleeps or timing assertions.
        self.executable.write_bytes(b"import time\ntime.sleep = lambda _: None\n" + SOURCE.read_bytes())
        self.children = []
        self.addCleanup(self.stop_children)

    def git(self, *args, repo=None):
        result = subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "-C", str(repo or self.repo), *args],
                                env=self.env, capture_output=True, text=True, check=True)
        return result.stdout

    def write_config(self):
        self.config_path.write_bytes(W.canonical(self.config))
        self.config_path.chmod(0o600)

    def status(self):
        return json.loads((self.root / "state/status.json").read_bytes())

    def start(self, background=False):
        process = subprocess.Popen([sys.executable, "-I", str(self.executable), "--config", str(self.config_path),
                                    *(["--background"] if background else [])], env=self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(process)
        return process

    def finish(self, process, status, code=None):
        stdout, stderr = process.communicate(timeout=20)
        self.assertEqual(process.returncode, (1 if status == "FAILED" else 0) if code is None else code, stderr)
        value = json.loads(stdout)
        W.validate(value, W.schema_documents()["merge-watch-status-v1.json"])
        self.assertEqual(value["status"], status, value)
        self.assertNotIn(b"SECRET", stdout + stderr)
        return value

    def stop_children(self):
        # Only our own foreground children or the PID from our private background status.
        for process in self.children:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=20)
        path = self.root / "state/status.json"
        if path.exists():
            pid = self.status()["pid"]
            if pid not in [p.pid for p in self.children] and self.status()["status"] not in W.TERMINAL:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

    def pr(self, merged=False):
        repository = {"id": 17, "full_name": "octo/project"}
        return {"number": 593, "head": {"sha": self.head, "repo": repository},
                "base": {"repo": repository}, "state": "closed" if merged else "open",
                "merged": merged, "merged_at": "2026-11-03T00:00:00Z" if merged else None,
                "merge_commit_sha": "a" * 40 if merged else None}

    def answer_pr(self, value=None):
        connection = self.service.take("/pulls/593")
        self.service.reply(connection, value if value is not None else self.pr())

    def answer_green(self, success=True):
        connection = self.service.take("/commits/" + self.head + "/check-runs?per_page=100")
        self.service.reply(connection, {"total_count": 1, "check_runs": [{"name": "CI", "head_sha": self.head,
                                  "status": "completed", "conclusion": "success" if success else "failure"}]})

    def merge_commit(self, squash=False, tree=None):
        connection = self.service.take("/git/commits/" + "a" * 40)
        self.service.reply(connection, {"sha": "a" * 40, "tree": {"sha": tree or self.head_tree},
                                       "parents": [{"sha": self.base}] if squash else
                                       [{"sha": self.base}, {"sha": self.head}]})

    def merge(self, squash=False):
        self.answer_pr(self.pr(merged=True))
        self.answer_green()
        self.merge_commit(squash=squash)
        self.answer_pr(self.pr(merged=True))

    def assert_cleaned(self):
        self.assertFalse(self.tree.exists())
        listing = self.git("worktree", "list", "--porcelain")
        self.assertNotIn(str(self.tree), listing)
        self.assertEqual(self.git("branch", "--list", "fix", "verification"), "")
        self.assertEqual((self.store / "events.sqlite").read_bytes(), b"audit evidence")

    def test_background_returns_green_wait_stays_alive_and_notifies_done_after_later_human_merge(self):
        parent = self.start(background=True)
        admitted = self.finish(parent, "STARTING")
        self.assertNotEqual(admitted["pid"], parent.pid)
        self.answer_pr()
        self.answer_green()
        self.answer_pr()
        # The next observation proves the same detached process continued after green CI.
        later = self.service.take("/pulls/593")
        waiting = self.status()
        self.assertEqual(waiting["status"], "WAITING_FOR_HUMAN_MERGE")
        self.assertEqual(waiting["pid"], admitted["pid"])
        self.assertTrue(self.tree.exists())
        duplicate = self.start()
        stdout, stderr = duplicate.communicate(timeout=20)
        self.assertEqual(duplicate.returncode, 1)
        self.assertIn(b"concurrent watcher", stderr)
        self.assertEqual(stdout, b"")
        # A different state directory cannot circumvent the repository/PR lock.
        self.config["state_dir"] = str(self.root / "other-state")
        self.write_config()
        duplicate = self.start()
        _, stderr = duplicate.communicate(timeout=20)
        self.assertEqual(duplicate.returncode, 1)
        self.assertIn(b"concurrent watcher", stderr)
        self.config["state_dir"] = str(self.root / "state")
        self.write_config()
        self.service.reply(later, self.pr(merged=True))
        self.answer_green()
        self.merge_commit()
        # A foreground replay will acquire the lock only after the detached worker terminates.
        # Synchronize on its lock, rather than waiting a guessed duration or polling status.
        self.answer_pr(self.pr(merged=True))
        with (self.root / "state/watch.lock").open() as lock:
            import fcntl
            fcntl.flock(lock, fcntl.LOCK_EX)
            final = self.status()
        self.assertEqual(final["status"], "DONE", final)
        self.assertTrue(all(item["phase"] == "done" for item in final["cleanup"]))
        self.assert_cleaned()
        # Terminal replay does not inspect GitHub or remove replacements at the same names.
        self.git("branch", "fix", self.head)
        self.tree.mkdir()
        (self.tree / "replacement").write_text("keep")
        replay = self.finish(self.start(), "DONE")
        self.assertEqual(replay, final)
        self.assertEqual((self.tree / "replacement").read_text(), "keep")
        self.assertTrue(self.git("branch", "--list", "fix"))

    def test_squash_exact_tree_is_verified_before_cleanup_and_done(self):
        process = self.start()
        self.merge(squash=True)
        final = self.finish(process, "DONE")
        self.assertEqual(final["merge_kind"], "exact_tree")
        self.assert_cleaned()

    def merged_checks_gate(self, during_poll, failure):
        process = self.start()
        self.answer_pr(self.pr(merged=not during_poll))
        if failure == "missing":
            connection = self.service.take("/commits/" + self.head + "/check-runs?per_page=100")
            self.service.reply(connection, {"total_count": 0, "check_runs": []})
        else:
            self.answer_green(success=False)
        if during_poll:
            self.answer_pr(self.pr(merged=True))
        # A new poll is a rendezvous after the prior poll's status was saved.
        pending = self.service.take("/pulls/593")
        state = self.status()
        self.assertEqual(state["status"], "WAITING_FOR_CI")
        self.assertEqual(state["merge_sha"], "")
        self.assertTrue(all(item["phase"] == "ready" for item in state["cleanup"]))
        self.assertTrue(self.tree.exists())
        self.assertIn(str(self.tree), self.git("worktree", "list", "--porcelain"))
        self.assertEqual(self.git("rev-parse", "refs/heads/fix").strip(), self.head)
        self.assertEqual(self.git("rev-parse", "refs/heads/verification").strip(), self.base)
        self.service.reply(pending, self.pr(merged=True))
        self.answer_green()
        self.merge_commit()
        self.answer_pr(self.pr(merged=True))
        self.finish(process, "DONE")
        self.assert_cleaned()

    def test_already_merged_missing_checks_preserve_targets_until_green(self):
        self.merged_checks_gate(during_poll=False, failure="missing")

    def test_already_merged_failed_checks_preserve_targets_until_green(self):
        self.merged_checks_gate(during_poll=False, failure="failed")

    def test_merge_during_poll_missing_checks_preserve_targets_until_green(self):
        self.merged_checks_gate(during_poll=True, failure="missing")

    def test_merge_during_poll_failed_checks_preserve_targets_until_green(self):
        self.merged_checks_gate(during_poll=True, failure="failed")

    def test_already_merged_checks_on_another_head_never_clean_up_or_finish_done(self):
        process = self.start()
        self.answer_pr(self.pr(merged=True))
        connection = self.service.take("/commits/" + self.head + "/check-runs?per_page=100")
        self.service.reply(connection, {"total_count": 1, "check_runs": [
            {"name": "CI", "head_sha": "b" * 40, "status": "completed", "conclusion": "success"}]})
        state = self.finish(process, "FAILED")
        self.assertIn("CI head mismatch", state["message"])
        self.assertTrue(all(item["phase"] == "ready" for item in state["cleanup"]))
        self.assertTrue(self.tree.exists())
        self.assertEqual(self.git("rev-parse", "refs/heads/fix").strip(), self.head)
        self.assertEqual(self.git("rev-parse", "refs/heads/verification").strip(), self.base)

    def test_head_change_and_closed_unmerged_are_terminal_and_preserve_targets(self):
        for reason in ("head", "closed"):
            with self.subTest(reason=reason):
                if (self.root / "state").exists():
                    shutil.rmtree(self.root / "state")
                process = self.start()
                pr = self.pr()
                if reason == "head":
                    pr["head"]["sha"] = "b" * 40
                else:
                    pr["state"] = "closed"
                self.answer_pr(pr)
                state = self.finish(process, "FAILED")
                self.assertIn("head changed" if reason == "head" else "closed without merge", state["message"])
                self.assertTrue(self.tree.exists())
                replay = self.finish(self.start(), "FAILED")
                self.assertEqual(state, replay)

    def test_merge_tree_or_identity_mismatch_preserves_targets(self):
        for reason in ("tree", "commit", "repository", "raced_head"):
            with self.subTest(reason=reason):
                if (self.root / "state").exists():
                    shutil.rmtree(self.root / "state")
                process = self.start()
                pr = self.pr(merged=True)
                if reason == "repository":
                    pr["base"]["repo"]["id"] = 999
                self.answer_pr(pr)
                if reason != "repository":
                    self.answer_green()
                if reason == "tree":
                    self.merge_commit(squash=True, tree="b" * 40)
                elif reason == "commit":
                    connection = self.service.take("/git/commits/" + "a" * 40)
                    self.service.reply(connection, {"sha": "b" * 40})
                elif reason == "raced_head":
                    self.merge_commit()
                    pr["head"]["sha"] = "b" * 40
                    self.answer_pr(pr)
                self.finish(process, "FAILED")
                self.assertTrue(self.tree.exists())

    def test_dirty_index_content_ignored_files_and_audit_store_refuse_cleanup(self):
        for reason in ("content", "index", "ignored", "store", "mode", "untracked", "assume_unchanged"):
            with self.subTest(reason=reason):
                if (self.root / "state").exists():
                    shutil.rmtree(self.root / "state")
                process = self.start()
                pending = self.service.take("/pulls/593")  # admission has verified the clean tree
                if reason == "content":
                    (self.tree / "file").write_text("uncommitted")
                elif reason == "index":
                    self.git("rm", "--cached", "file", repo=self.tree)
                elif reason == "ignored":
                    (self.tree / "target").mkdir()
                    (self.tree / "target/build").write_text("keep")
                elif reason == "store":
                    (self.tree / "events.sqlite").write_bytes(b"keep audit")
                elif reason == "mode":
                    (self.tree / "file").chmod(0o755)
                elif reason == "untracked":
                    (self.tree / "untracked").write_text("keep")
                else:
                    self.git("update-index", "--assume-unchanged", "file", repo=self.tree)
                    (self.tree / "file").write_text("hidden dirty change")
                self.service.reply(pending, self.pr(merged=True))
                self.answer_green()
                self.merge_commit()
                self.answer_pr(self.pr(merged=True))
                final = self.finish(process, "FAILED")
                self.assertTrue(self.tree.exists())
                self.assertTrue(all(item["phase"] == "ready" for item in final["cleanup"]))
                if reason == "assume_unchanged":
                    self.git("update-index", "--no-assume-unchanged", "file", repo=self.tree)
                self.git("reset", "--hard", self.head, repo=self.tree)
                if reason == "ignored":
                    shutil.rmtree(self.tree / "target")
                if reason == "store":
                    (self.tree / "events.sqlite").unlink()
                if reason == "untracked":
                    (self.tree / "untracked").unlink()

    def test_changed_branch_tip_never_deleted_or_done(self):
        process = self.start()
        pending = self.service.take("/pulls/593")
        self.git("branch", "-f", "verification", self.head)
        self.service.reply(pending, self.pr(merged=True))
        self.answer_green()
        self.merge_commit()
        self.answer_pr(self.pr(merged=True))
        state = self.finish(process, "FAILED")
        self.assertIn("branch tip changed", state["message"])
        self.assertFalse(self.tree.exists())
        self.assertTrue(self.git("branch", "--list", "verification"))
        self.assertNotEqual(state["status"], "DONE")

    def test_hooks_filters_and_fsmonitor_are_never_executed(self):
        sentinel = self.root / "executed"
        malicious = self.root / "malicious"
        malicious.write_text("#!/bin/sh\ntouch '" + str(sentinel) + "'\ncat\n")
        malicious.chmod(0o700)
        included = self.root / "included-config"
        included.write_text('[filter "trap"]\n clean = ' + str(malicious) + '\n process = ' + str(malicious) + '\n')
        self.git("config", "include.path", str(included))
        self.git("config", "extensions.worktreeConfig", "true")
        self.git("config", "--worktree", "filter.worktree-trap.clean", str(malicious), repo=self.tree)
        self.git("config", "--worktree", "filter.worktree-trap.process", str(malicious), repo=self.tree)
        self.git("config", "core.fsmonitor", str(malicious))
        (self.repo / ".git/info/attributes").write_text("* filter=trap\nfile filter=worktree-trap\n")
        (self.repo / ".git/hooks/post-checkout").write_text(malicious.read_text())
        (self.repo / ".git/hooks/post-checkout").chmod(0o700)
        process = self.start()
        self.merge()
        self.finish(process, "DONE")
        self.assertFalse(sentinel.exists())
        self.assert_cleaned()

    def test_restart_reconciles_completed_intent_and_never_repeats_uncertain_delete(self):
        process = self.start()
        pending = self.service.take("/pulls/593")
        process.kill()
        process.communicate(timeout=20)
        pending.close()
        state = self.status()
        state.update(status="CLEANING", merge_sha="a" * 40, merge_kind="ancestry")
        state["cleanup"][0]["phase"] = "intent"
        self.git("worktree", "remove", str(self.tree))
        self.persist(state)
        resumed = self.start()
        self.merge()
        self.finish(resumed, "DONE")
        self.assert_cleaned()
        # Restore the exact names for a second interrupted intent, but not the old inode.
        self.git("worktree", "add", "-b", "fix", str(self.tree), self.head)
        self.git("branch", "verification", self.base)
        state["cleanup"][0]["phase"] = "done"
        state["cleanup"][1]["phase"] = "intent"
        self.persist(state)
        resumed = self.start()
        self.merge()
        final = self.finish(resumed, "FAILED")
        self.assertIn("recreated", final["message"])
        self.assertTrue(self.tree.exists())
        self.assertTrue(self.git("branch", "--list", "fix"))

    def interrupted_branch_intent(self):
        process = self.start()
        pending = self.service.take("/pulls/593")
        process.kill()
        process.communicate(timeout=20)
        pending.close()
        state = self.status()
        state.update(status="CLEANING", merge_sha="a" * 40, merge_kind="ancestry")
        self.git("worktree", "remove", str(self.tree))
        state["cleanup"][0]["phase"] = "done"
        state["cleanup"][1]["phase"] = "intent"
        return state

    def test_existing_branch_with_uncertain_intent_is_preserved_without_repeating_delete(self):
        self.persist(self.interrupted_branch_intent())
        process = self.start()
        self.merge()
        final = self.finish(process, "FAILED")
        self.assertIn("uncertain cleanup intent", final["message"])
        self.assertTrue(self.git("branch", "--list", "fix"))
        self.assertTrue(self.git("branch", "--list", "verification"))
        self.assertEqual(self.finish(self.start(), "FAILED"), final)

    def test_absent_branch_with_uncertain_intent_is_reconciled_and_remaining_cleanup_finishes(self):
        state = self.interrupted_branch_intent()
        self.git("update-ref", "-d", "refs/heads/fix", self.head)
        self.persist(state)
        process = self.start()
        self.merge()
        self.finish(process, "DONE")
        self.assert_cleaned()

    def test_tracked_audit_store_is_refused_even_when_the_worktree_is_clean(self):
        (self.tree / "events.sqlite").write_bytes(b"retain tracked audit")
        self.git("add", "events.sqlite", repo=self.tree)
        self.git("commit", "-m", "audit fixture", repo=self.tree)
        head = self.git("rev-parse", "HEAD", repo=self.tree).strip()
        self.config["head"] = head
        self.config["worktrees"][0]["head"] = head
        self.config["branches"][0]["head"] = head
        self.write_config()
        process = self.start()
        stdout, stderr = process.communicate(timeout=20)
        self.assertEqual(process.returncode, 1)
        self.assertEqual(stdout, b"")
        self.assertIn(b"audit Store", stderr)
        self.assertEqual((self.tree / "events.sqlite").read_bytes(), b"retain tracked audit")

    def persist(self, state):
        state["checksum"] = W.digest({k: v for k, v in state.items() if k != "checksum"})
        (self.root / "state/status.json").write_bytes(W.canonical(state))
        (self.root / "state/status.json").chmod(0o600)

    def test_poll_and_error_budgets_survive_restart(self):
        self.config["max_polls"] = 1
        self.write_config()
        process = self.start()
        self.answer_pr()
        self.answer_green()
        self.answer_pr()
        failed = self.finish(process, "FAILED")
        self.assertEqual(failed["polls"], 1)
        self.assertIn("poll budget exhausted", failed["message"])
        self.assertEqual(self.finish(self.start(), "FAILED"), failed)
        shutil.rmtree(self.root / "state")
        self.config["max_polls"] = 10
        self.write_config()
        process = self.start()
        for _ in range(2):
            connection = self.service.take("/pulls/593")
            self.service.reply(connection, {"error": "REMOTE-SECRET"})
        failed = self.finish(process, "FAILED")
        self.assertEqual(failed["errors"], 2)
        self.assertTrue(self.tree.exists())

    def test_termination_stops_current_observation_and_resume_keeps_budget(self):
        process = self.start()
        pending = self.service.take("/pulls/593")
        process.terminate()
        stdout, stderr = process.communicate(timeout=20)
        pending.close()
        self.assertEqual(process.returncode, 130)
        self.assertEqual(stdout, b"")
        self.assertIn(b"resume", stderr)
        prior = self.status()
        self.assertNotIn(prior["status"], W.TERMINAL)
        resumed = self.start()
        self.merge()
        final = self.finish(resumed, "DONE")
        self.assertEqual(final["polls"], prior["polls"] + 1)
        self.assertEqual(final["deadline_unix"], prior["deadline_unix"])
        self.assert_cleaned()

    def test_runtime_expiry_is_terminal_without_extending_deadline_on_restart(self):
        process = self.start()
        pending = self.service.take("/pulls/593")
        process.kill()
        process.communicate(timeout=20)
        pending.close()
        state = self.status()
        state["deadline_unix"] = 1
        self.persist(state)
        failed = self.finish(self.start(), "FAILED")
        self.assertIn("runtime exhausted", failed["message"])
        self.assertEqual(failed["deadline_unix"], 1)
        self.assertTrue(self.tree.exists())

    def test_schema_parity_unknown_versions_and_corruption_fail_closed(self):
        for name, shape in W.schema_documents().items():
            self.assertEqual((ROOT / "schemas" / name).read_text(), json.dumps(shape, indent=2, sort_keys=True) + "\n")
        with self.assertRaises(W.Refused):
            W.validate({**self.config, "schema": "af/merge-watch-config@0"}, W.schema_documents()["merge-watch-config-v1.json"])
        process = self.start()
        pending = self.service.take("/pulls/593")
        process.kill()
        process.communicate(timeout=20)
        pending.close()
        original = self.status()
        for change in ({"schema": "af/merge-watch-status@0"}, {"checksum": "f" * 64},
                       {"status": "DONE"}, {"polls": 20001}):
            with self.subTest(change=change):
                state = copy.deepcopy(original)
                state.update(change)
                if "checksum" not in change:
                    self.persist(state)
                else:
                    (self.root / "state/status.json").write_bytes(W.canonical(state))
                resumed = self.start()
                stdout, _ = resumed.communicate(timeout=20)
                self.assertEqual(resumed.returncode, 1)
                self.assertEqual(stdout, b"")
                self.assertTrue(self.tree.exists())

    def test_command_output_and_deadlines_are_bounded_without_leaking_remote_bytes(self):
        with self.assertRaisesRegex(W.Refused, "output exceeds"):
            W.command([sys.executable, "-c", "print('REMOTE-SECRET' * 10000)"], self.env,
                      W.time.monotonic() + 10, limit=64)
        with self.assertRaisesRegex(W.Refused, "runtime exhausted"):
            W.command([sys.executable, "-c", "raise AssertionError('must not execute')"], self.env, 0)
        with patch.object(W.time, "monotonic", side_effect=[0, 0, 20]):
            with self.assertRaisesRegex(W.Unavailable, "deadline exceeded"):
                W.command([sys.executable, "-c", "pass"], self.env, 10)


if __name__ == "__main__":
    unittest.main()
