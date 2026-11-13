#!/usr/bin/env python3
"""Hermetic lifecycle tests: real Git, fake native source transport and fake GitHub; no publication."""
import copy
import contextlib
import io
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def fixture_launcher(body):
    """Use current canonical Python on Darwin, system Python elsewhere, always isolated.

    Kernels split a shebang at whitespace, so refuse unrepresentable Darwin paths
    rather than silently selecting another Python. -IS is one kernel argument for -I -S.
    This test-only policy never changes the installed adapter policy.
    """
    if sys.platform != "darwin":
        return "#!/usr/bin/python3 -IS\n" + body
    python = Path(sys.executable).resolve(strict=True)
    if not python.is_absolute() or any(c.isspace() for c in str(python)):
        raise ValueError("workspace fixture Python path must be absolute and contain no whitespace: " + str(python))
    line = "#!" + str(python) + " -IS\n"
    if len(line.encode()) > 127:
        raise ValueError("workspace fixture Python shebang exceeds the portable 127-byte limit: " + str(python))
    return line + body


def performance_git_launcher(git, body):
    """Intercept only the performance fixture's lost responses and remote transport.

    The publish step's 30 s deadline includes every reconciliation. Starting Python for
    dozens of ordinary Git reads consumed that budget on macOS, even in isolation. Keep
    the entire interceptor in the pinned executable, but exec ordinary Git directly.
    The three intercepted commands still run the same isolated Python and real Git.
    Any use of the fixture's remote URL also enters Python's fail-closed route check.
    """
    python = fixture_launcher("").splitlines()[0][2:].split()
    return ("#!/bin/sh\nfor argument do\n case \"$argument\" in\n"
            " commit-tree|push|ls-remote|ssh://git@github.com/octo/project.git)\n  exec "
            + shlex.join([*python, "-c", body])
            + ' "$@"\n ;;\n esac\ndone\nexec ' + shlex.quote(str(git)) + ' "$@"\n')


I = module("workspace_installer", ROOT / "scripts/install-scenario-git-workspace.py")
A = I.module()

D = "sha256:" + "d" * 64
S = "sha256:" + "e" * 64
R = "sha256:" + "f" * 64


class Service:
    def __init__(self):
        self.prs, self.refs, self.posts, self.pushes = [], {}, 0, 0
        self.crash_post, self.lose_post = False, False
        self.reviews = []
        self.green = True
        self.source = None

    def request(self, method, path, body=None):
        if method == "GET" and path == "/user":
            return dict(id=7, login="owner")
        if method == "GET" and path == "/repos/octo/project":
            return dict(id=17, full_name="octo/project")
        if method == "POST" and path == "/repos/octo/project/pulls":
            self.posts += 1
            p = self.source
            pr = dict(id=100 + self.posts, number=self.posts, user=dict(id=7, login="owner"), title=body["title"], body=body["body"],
                url="https://api.github.com/repos/octo/project/pulls/" + str(self.posts),
                html_url="https://github.com/octo/project/pull/" + str(self.posts),
                head=dict(ref=body["head"], sha=p["commit"], repo=dict(id=17, full_name="octo/project")),
                base=dict(ref=body["base"], repo=dict(id=17, full_name="octo/project")), state="open", merged=False)
            if not self.lose_post:
                self.prs.append(pr)
            if self.crash_post or self.lose_post:
                raise RuntimeError("transport lost after possible POST")
            return copy.deepcopy(pr)
        if method == "GET" and path.startswith("/repos/octo/project/pulls/"):
            number = int(path.rsplit("/", 1)[1])
            return copy.deepcopy(next(pr for pr in self.prs if pr["number"] == number))
        raise AssertionError((method, path))

    def pages(self, path, key=None, params=None):
        if path == "/repos/octo/project/pulls":
            return copy.deepcopy(self.prs)
        if path.endswith("/reviews"):
            return copy.deepcopy(self.reviews)
        if path.endswith("/check-runs"):
            return [dict(id=50, name="CI", app=dict(id=4), head_sha=self.source["commit"],
                         check_suite=dict(id=40), status="completed", conclusion="success" if self.green else "failure")]
        if path.endswith("/check-suites"):
            return [dict(id=40, app=dict(id=4), head_sha=self.source["commit"], status="completed",
                         conclusion="success" if self.green else "failure")]
        raise AssertionError(path)


class LocalAdapter(A.WorkspaceAdapter):
    """Only the external Git SSH transport is replaced. All local Git and journal code is production."""
    def remote_ref(self, p):
        return self.service.refs.get(p["branch"])

    def git(self, *args, **kwargs):
        if args[0] == "push":
            self.service.pushes += 1
            target = args[-1]
            commit, ref = target.split(":", 1)
            branch = ref.removeprefix("refs/heads/")
            expected = args[2].split(":", 1)[1]
            self.assert_lease(self.service.refs.get(branch), expected)
            if commit:
                self.service.refs[branch] = commit
                for pr in self.service.prs:
                    if pr["head"]["ref"] == branch:
                        pr["head"]["sha"] = commit
            else:
                self.service.refs.pop(branch, None)
            return b""
        return super().git(*args, **kwargs)

    @staticmethod
    def assert_lease(current, expected):
        A.need(current == (expected or None), "fake transport exact lease failed")


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.root.chmod(0o700)
        self.home = self.root / "test-home"
        self.home.mkdir(mode=0o700)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.env = dict(PATH=os.defpath, HOME=str(self.home), XDG_CONFIG_HOME=str(self.home),
                        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
                        GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="test@example.invalid",
                        GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="test@example.invalid")
        git = shutil.which("git", path=self.env["PATH"])
        self.assertIsNotNone(git, "Git must be available on the fixture's hermetic PATH")
        self.git_path = Path(git).resolve()
        self.git("init", "-q")
        (self.repo / ".gitignore").write_text("target/\nforeign/\n")
        (self.repo / "old").write_text("before\n")
        self.git("add", ".")
        self.git("commit", "-qm", "source")
        source_commit = self.git("rev-parse", "HEAD")
        # A fake native source transport supplies a real candidate Git tree. Native receipt
        # admission itself is tested through af in scenario::implementation_* Rust tests.
        (self.repo / "old").unlink()
        (self.repo / "new").write_text("candidate\n")
        (self.repo / "new").chmod(0o755)
        (self.repo / "link").symlink_to("new")
        self.git("add", "-A")
        self.git("commit", "-qm", "fake accepted candidate")
        tree = self.git("rev-parse", "HEAD^{tree}")
        transport = self.git("rev-parse", "HEAD")
        self.git("branch", "af-candidate", transport)
        self.git("reset", "--hard", source_commit)
        task_store = self.root / "tasks"
        task_store.mkdir(mode=0o700)
        worktrees = self.root / "worktrees"
        worktrees.mkdir(mode=0o700)
        token = self.root / "token"
        token.write_text("github_pat_TEST_CREDENTIAL_ONLY\n")
        token.chmod(0o600)
        ssh = self.root / "ssh-config"
        ssh.write_text("Host github.com\n  BatchMode yes\n")
        ssh.chmod(0o600)
        af = self.root / "fake-af"
        native = dict(schema="af/git-candidate-source@1", repository_id="fixture", source_snapshot=S,
                      candidate_snapshot=R, predecessor_snapshot=None, source_content_digest=D, source_commit=source_commit,
                      candidate_tree=tree, transport_commit=transport)
        af.write_text(fixture_launcher('''import hashlib, json, pathlib, sys, subprocess, os
args=sys.argv[1:]
if args[:2] == ["task", "materialize"]:
 dest=pathlib.Path(args[args.index("--into")+1])
 repo=args[args.index("--repo")+1]
 listing=subprocess.check_output(["/usr/bin/git","-C",repo,"ls-tree","-rz",subprocess.check_output(["/usr/bin/git","-C",str(dest),"rev-parse","HEAD"]).decode().strip()])
 for record in listing.split(b"\\0"):
  if not record: continue
  metadata,name=record.split(b"\\t",1)
  mode,kind,oid=metadata.split()
  content=subprocess.check_output(["/usr/bin/git","-C",repo,"cat-file","blob",oid.decode()])
  path=dest / os.fsdecode(name)
  path.parent.mkdir(parents=True,exist_ok=True)
  if mode==b"120000": path.symlink_to(os.fsdecode(content))
  else:
   path.write_bytes(content)
   path.chmod(0o755 if mode==b"100755" else 0o644)
 print(json.dumps(dict(schema="af/task-materialized@1",task_id=args[2],snapshot_id=args[args.index("--snapshot")+1],verified=True)))
 sys.exit(0)
assert args[:2] == ["scenario", "candidate-source"]
values=dict(zip(args[2:-1:2],args[3:-1:2]))
assert args[-1] == "--json"
request=json.loads(pathlib.Path(values["--file"]).read_text())
assert request["schema"] == "af/git-candidate-source-request@1"
source=''' + repr(native) + '''
source["request_digest"]="sha256:"+hashlib.sha256(json.dumps(request,sort_keys=True,separators=(",", ":"),ensure_ascii=False).encode()).hexdigest()
# Reuse the fixture repository's already pinned af-candidate ref; no real af is spawned.
source["repository"]=values["--into"]+"/repository.git"
p=pathlib.Path(values["--into"])
p.mkdir(exist_ok=True)
(p/"repository.git").symlink_to(pathlib.Path(values["--repo"])/".git") if not (p/"repository.git").exists() else None
print(json.dumps(source))
'''))
        af.chmod(0o700)
        git_path = self.git_path
        self.config = dict(schema=A.WORKSPACE_CONFIG, repository="octo/project", repository_id=17,
            actor=dict(id=7, login="owner"), token_file=str(token), state_dir=str(self.root / "state"),
            repo=str(self.repo), task_store=str(task_store), worktree_root=str(worktrees), base="main",
            branch_prefix="work", source_commit=source_commit, source_content_digest=D,
            af=str(af), af_digest=A.digest(af.read_bytes()), git=str(git_path), git_digest=A.digest(git_path.read_bytes()),
            ssh_config=str(ssh), ssh_config_digest=A.digest(ssh.read_bytes()), author_name="Owner", author_email="owner@example.invalid",
            required_checks=[dict(kind="check", name="CI", app_id=4)], reviewers=[dict(id=8, login="reviewer")], publication_mode="exclusive_owned_writer", operations=["prepare_candidate_v1", "cleanup_workspace_v1", "publish_pr", "observe"])
        self.config_path = self.root / "config.json"
        self.config_path.write_bytes(A.canonical(self.config))
        self.config_path.chmod(0o600)
        self.config_digest = A.digest(self.config_path.read_bytes())
        A.CONFIG_DIGEST = self.config_digest
        self.executable = self.root / "adapter"
        with contextlib.redirect_stdout(io.StringIO()):
            I.install(self.config_path, self.executable)
        self.service = Service()
        self.candidate = dict(task_id="refactor", plan_id=D, revision_id=S, result_id=D, charged_tokens=1,
            result=dict(task_revision_id=S, execution="completed", acceptance="satisfied", domain_conclusion="verified",
                outputs=dict(snapshot=dict(artifact_type="af/SourceTree@1", cardinality="one", artifact_ids=[D], snapshot_id=R)),
                evidence=[D], missing_obligations=[]))
        self.prepare_req = self.request("prepare_candidate_v1", "a", dict(subject=self.text(D), revision=self.text(R),
            body=self.text(A.canonical(dict(schema="af/git-workspace-content@1", title="Implement CLI", body="Exact approved body")).decode()),
            candidate=dict(type="task", value=self.candidate)))

    def git(self, *args):
        p = subprocess.run([str(self.git_path), *args], cwd=self.repo, env=self.env, capture_output=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout.decode().strip()

    @staticmethod
    def text(value):
        return dict(type="text", value=value)

    def request(self, op, key, inputs, method="execute"):
        return dict(schema="af/scenario-adapter-request@4", operation=op, intent_id="sha256:" + key * 64,
                    input_digest=D, destination="github:octo/project", method=method, inputs=inputs)

    def adapter(self):
        return LocalAdapter(self.config, self.config_digest, self.service)

    def call(self, r, method=None):
        r = copy.deepcopy(r)
        if method:
            r["method"] = method
        return self.adapter().handle(r)

    def prepare(self):
        self.assertEqual(self.call(self.prepare_req, "reconcile")["outcome"], "absent")
        out = self.call(self.prepare_req)
        self.assertEqual(out["outcome"], "done")
        p = json.loads(out["evidence"])
        self.service.source = p
        return p

    def interrupt_empty_worktree(self):
        save = A.WorkspaceJournal.save
        def crash(journal):
            save(journal)
            rec = journal.state["records"].get(self.prepare_req["intent_id"])
            if rec and rec["reservations"]["worktree"]["published"]:
                raise RuntimeError("crash after empty directory journal save")
        with patch.object(A.WorkspaceJournal, "save", crash):
            with self.assertRaisesRegex(RuntimeError, "empty directory journal save"):
                self.call(self.prepare_req)
        adapter = self.adapter()
        with adapter.journal.locked():
            rec = copy.deepcopy(adapter.journal.state["records"][self.prepare_req["intent_id"]])
        path = Path(rec["prepared"]["worktree"])
        self.assertEqual(list(path.iterdir()), [])
        return rec, path

    def test_prepare_resumes_same_empty_directory_after_journal_crash(self):
        rec, path = self.interrupt_empty_worktree()
        result = self.call(self.prepare_req, "reconcile")
        self.assertEqual(result["outcome"], "done")
        self.assertEqual(json.loads(result["evidence"]), rec["prepared"])
        self.assertEqual((path.stat().st_dev, path.stat().st_ino), (rec["device"], rec["inode"]))
        self.assertEqual((path / "new").read_text(), "candidate\n")
        self.assertEqual(self.call(self.prepare_req)["evidence"], result["evidence"])
        self.assertEqual((self.service.posts, self.service.pushes), (0, 0))

    def test_prepare_retains_changed_empty_directory_and_partial_content(self):
        rec, path = self.interrupt_empty_worktree()
        # Renaming keeps the original inode live so a replacement cannot reuse it.
        original = path.with_name(path.name + "-original")
        path.rename(original)
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        self.assertFalse(path.exists())
        path.mkdir(mode=0o700)
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        self.assertEqual(list(path.iterdir()), [])
        path.rmdir()
        path.symlink_to(original, target_is_directory=True)
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        path.unlink()
        original.rename(path)
        path.chmod(0o755)
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        path.chmod(0o700)
        for name in ("foreign", ".git"):
            (path / name).write_text("partial or foreign bytes")
            with self.assertRaises(A.Refused):
                self.call(self.prepare_req, "reconcile")
            self.assertEqual((path / name).read_text(), "partial or foreign bytes")
            (path / name).unlink()
        # Git can have registered an interrupted checkout even when the directory is empty.
        self.git("worktree", "add", "--detach", str(path), rec["prepared"]["commit"])
        for entry in path.iterdir():
            entry.unlink()
        registered = self.git("worktree", "list", "--porcelain")
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        self.assertEqual(list(path.iterdir()), [])
        self.assertEqual(self.git("worktree", "list", "--porcelain"), registered)

    def reservation_crash(self, kind, event, resume=True):
        active = None
        fired = False
        renamed = False
        reserve = A.WorkspaceAdapter.reserve_directory
        mkdir, sync, publish, save = Path.mkdir, A.sync_directory, A.publish_directory, A.WorkspaceJournal.save

        def checkpoint(name, when):
            nonlocal fired
            if active == kind and event == name + "." + when and not fired:
                fired = True
                raise RuntimeError("injected reservation interruption")

        def reserving(adapter, rec, current, destination):
            nonlocal active, renamed
            renamed = False
            active = current
            try:
                return reserve(adapter, rec, current, destination)
            finally:
                active = None

        def creating(path, *args, **kwargs):
            if path.name.startswith(".reserve-"):
                checkpoint("create", "before")
                result = mkdir(path, *args, **kwargs)
                checkpoint("create", "after")
                return result
            return mkdir(path, *args, **kwargs)

        def syncing(path):
            name = ("sync_stage" if path.name.startswith(".reserve-") else
                    "sync_root" if path == Path(self.config["worktree_root"]) and not renamed else "sync_final")
            checkpoint(name, "before")
            sync(path)
            checkpoint(name, "after")

        def publishing(source, destination):
            nonlocal renamed
            checkpoint("publish", "before")
            publish(source, destination)
            renamed = True
            checkpoint("publish", "after")

        def saving(journal):
            rec = journal.state["records"].get(self.prepare_req["intent_id"])
            name = None
            if active == kind and rec:
                owned = rec["inode"] is not None if kind == "worktree" else rec["build"] is not None
                if owned:
                    name = "completion" if rec["reservations"][kind]["published"] else "ownership"
            if name:
                checkpoint(name, "before")
            save(journal)
            if name:
                checkpoint(name, "after")

        with patch.object(A.WorkspaceAdapter, "reserve_directory", reserving), \
             patch.object(Path, "mkdir", creating), patch.object(A, "sync_directory", syncing), \
             patch.object(A, "publish_directory", publishing), patch.object(A.WorkspaceJournal, "save", saving):
            with self.assertRaisesRegex(RuntimeError, "injected reservation interruption"):
                self.call(self.prepare_req)
        self.assertTrue(fired)
        adapter = self.adapter()
        with adapter.journal.locked():
            retained = copy.deepcopy(adapter.journal.state["records"][self.prepare_req["intent_id"]])
            expected = retained["prepared"]
        saved_identity = ({"device": retained["device"], "inode": retained["inode"]}
                          if kind == "worktree" and retained["inode"] is not None else
                          retained["build"] if kind == "build" else None)
        uncertain = {path: path.stat().st_ino for path in Path(self.config["worktree_root"]).glob(".reserve-*")}
        if not resume:
            return expected
        result = self.call(self.prepare_req, "reconcile")
        self.assertEqual(result["outcome"], "done")
        self.assertEqual(json.loads(result["evidence"]), expected)
        self.assertEqual(self.call(self.prepare_req)["evidence"], result["evidence"])
        self.assertEqual((Path(expected["worktree"]) / "new").read_text(), "candidate\n")
        final = Path(expected["worktree"]) / "target" if kind == "build" else Path(expected["worktree"])
        if saved_identity is not None:
            self.assertEqual((final.stat().st_dev, final.stat().st_ino),
                             (saved_identity["device"], saved_identity["inode"]))
        else:
            self.assertEqual({path: path.stat().st_ino for path in uncertain}, uncertain)
        stages = list(Path(self.config["worktree_root"]).glob(".reserve-*"))
        self.assertLessEqual(len(stages), 1)
        self.assertTrue(all(not list(stage.iterdir()) for stage in stages))
        self.assertEqual((self.service.posts, self.service.pushes), (0, 0))

    def changed_reservation(self, kind, event):
        expected = self.reservation_crash(kind, event, resume=False)
        root = Path(self.config["worktree_root"])
        path = (root / (".reserve-" + "a" * 64 + "-" + kind + "-0") if event == "ownership.after"
                else Path(expected["worktree"]) / "target" if kind == "build" else Path(expected["worktree"]))
        original = path.with_name(path.name + "-original")
        path.rename(original)
        path.mkdir(mode=0o700)
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        self.assertTrue(path.is_dir())
        path.rmdir()
        path.symlink_to(original, target_is_directory=True)
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        self.assertTrue(path.is_symlink())
        path.unlink()
        original.rename(path)
        path.chmod(0o755)
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        path.chmod(0o700)
        (path / "foreign").write_text("preserve")
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        self.assertEqual((path / "foreign").read_text(), "preserve")
        (path / "foreign").unlink()
        self.assertEqual(self.call(self.prepare_req, "reconcile")["outcome"], "done")

    def foreign_destination(self, kind):
        expected = self.reservation_crash(kind, "ownership.after", resume=False)
        destination = Path(expected["worktree"])
        if kind == "build":
            destination /= "target"
        destination.mkdir(mode=0o700)
        inode = destination.stat().st_ino
        with self.assertRaises((A.Refused, OSError)):
            self.call(self.prepare_req, "reconcile")
        self.assertEqual(destination.stat().st_ino, inode)
        self.assertEqual(list(destination.iterdir()), [])
        destination.rmdir()
        self.assertEqual(self.call(self.prepare_req, "reconcile")["outcome"], "done")

    def uncertain_slot_crash(self, kind, when):
        stage = Path(self.config["worktree_root"]) / (".reserve-" + "a" * 64 + "-" + kind + "-0")
        stage.mkdir(mode=0o700)
        inode = stage.stat().st_ino
        save = A.WorkspaceJournal.save
        def saving(journal):
            rec = journal.state["records"].get(self.prepare_req["intent_id"])
            if rec and rec["reservations"][kind]["slot"] == 1:
                if when == "after":
                    save(journal)
                raise RuntimeError("slot advancement interruption")
            save(journal)
        with patch.object(A.WorkspaceJournal, "save", saving):
            with self.assertRaisesRegex(RuntimeError, "slot advancement interruption"):
                self.call(self.prepare_req)
        self.assertEqual(self.call(self.prepare_req, "reconcile")["outcome"], "done")
        self.assertEqual(stage.stat().st_ino, inode)
        self.assertEqual(list(stage.iterdir()), [])
        self.assertEqual(list(Path(self.config["worktree_root"]).glob(".reserve-*")), [stage])

    def uncertain_symlink(self, kind):
        self.reservation_crash(kind, "create.before", resume=False)
        stage = Path(self.config["worktree_root"]) / (".reserve-" + "a" * 64 + "-" + kind + "-0")
        stage.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        self.assertTrue(stage.is_symlink())
        stage.unlink()
        stage.mkdir(mode=0o700)
        (stage / "foreign").write_text("preserve")
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req, "reconcile")
        self.assertEqual((stage / "foreign").read_text(), "preserve")

    def test_prepare_completion_journal_crash(self):
        save = A.WorkspaceJournal.save
        for when in ("before", "after"):
            # The first iteration resumes through the completion write; the second
            # uses that same completed intent to exercise lost completion response.
            def saving(journal):
                rec = journal.state["records"].get(self.prepare_req["intent_id"])
                if rec and rec["phase"] == "done":
                    if when == "after":
                        save(journal)
                    raise RuntimeError("completion interruption")
                save(journal)
            if when == "before":
                with patch.object(A.WorkspaceJournal, "save", saving):
                    with self.assertRaisesRegex(RuntimeError, "completion interruption"):
                        self.call(self.prepare_req)
            else:
                with patch.object(A.WorkspaceJournal, "save", saving):
                    with self.assertRaisesRegex(RuntimeError, "completion interruption"):
                        self.call(self.prepare_req, "reconcile")
        self.assertEqual(self.call(self.prepare_req, "reconcile")["outcome"], "done")

    def test_exclusive_publication_preserves_foreign_empty_destination(self):
        source, destination = self.root / "staged", self.root / "foreign"
        source.mkdir(mode=0o700)
        destination.mkdir(mode=0o700)
        identities = source.stat().st_ino, destination.stat().st_ino
        with self.assertRaises(OSError):
            A.publish_directory(source, destination)
        self.assertEqual((source.stat().st_ino, destination.stat().st_ino), identities)

    def test_uncertain_staging_is_bounded_and_never_adopted_or_deleted(self):
        root = Path(self.config["worktree_root"])
        stages = [root / (".reserve-" + "a" * 64 + "-worktree-" + str(n)) for n in range(A.RESERVATION_SLOTS)]
        for stage in stages:
            stage.mkdir(mode=0o700)
        identities = [stage.stat().st_ino for stage in stages]
        for _ in range(2):
            with self.assertRaisesRegex(A.Refused, "reservation budget exhausted"):
                self.call(self.prepare_req)
        self.assertEqual([stage.stat().st_ino for stage in stages], identities)
        self.assertEqual(len(list(root.iterdir())), A.RESERVATION_SLOTS)
        self.assertFalse((root / ("a" * 64)).exists())

    def test_prepare_deterministic_exact_tree_and_restart_reuses_owned_commit(self):
        p = self.prepare()
        path = Path(p["worktree"])
        self.assertFalse((path / "old").exists())
        self.assertEqual((path / "new").read_text(), "candidate\n")
        self.assertTrue((path / "new").stat().st_mode & 0o100)
        self.assertEqual(os.readlink(path / "link"), "new")
        self.assertEqual(self.git("rev-parse", p["commit"] + "^{tree}"), p["tree"])
        self.assertEqual(self.git("rev-parse", p["commit"] + "^"), p["source_commit"])
        self.assertEqual(json.loads(self.call(self.prepare_req, "reconcile")["evidence"]), p)
        self.assertEqual(self.service.posts, 0)
        self.assertEqual(self.git("rev-parse", "HEAD"), p["source_commit"])

    def test_escaped_publication_bound_is_checked_before_branch_or_worktree_creation(self):
        r = copy.deepcopy(self.prepare_req)
        content = json.loads(r["inputs"]["body"]["value"])
        content["body"] = '"' * 30000
        r["inputs"]["body"] = self.text(A.canonical(content).decode())
        with self.assertRaises(A.Refused):
            self.call(r)
        self.assertFalse(list(Path(self.config["worktree_root"]).iterdir()))
        self.assertFalse(self.git("for-each-ref", "--format=%(refname)", "refs/heads/impl/"))
        self.assertEqual(self.service.posts, 0)

    def test_native_refusal_never_materializes_or_creates_a_branch(self):
        with patch.object(LocalAdapter, "native_source", side_effect=A.Refused("unaccepted or foreign source")):
            with self.assertRaises(A.Refused):
                self.call(self.prepare_req)
        self.assertFalse(list(Path(self.config["worktree_root"]).iterdir()))
        self.assertFalse(self.git("for-each-ref", "--format=%(refname)", "refs/heads/impl/"))

    def action(self, p, op="publish_pr", key="b"):
        receipt = dict(intent_id=p["owner"], operation="prepare_candidate_v1", destination="github:octo/project",
                       subject=p["result_id"], revision=p["snapshot_id"], evidence=A.canonical(p).decode())
        inputs = dict(subject=self.text(A.prepared_subject(p)), revision=self.text(p["commit"]),
                      body=self.text(A.canonical(A.publication_payload(p) if op == "publish_pr" else p).decode()))
        if op == "publish_pr":
            inputs["decision"] = dict(type="decision", value=dict(schema="af/scenario-signed-decision@1", signature="fixture signature verified by kernel",
                request=dict(schema="af/scenario-decision@1", run_id="refactor", execution_id=D, pin_id=S, step="root/publish",
                             subject=A.prepared_subject(p), revision=p["commit"], owner="owner", approve=True, expires_unix_ms=9999999999999)))
        else:
            inputs["prepared"] = dict(type="action", value=receipt)
            inputs["evidence"] = dict(type="observation", value=dict(subject=A.prepared_subject(p), revision=p["commit"], condition="merged", satisfied=True, evidence="kernel merge Wait"))
        return self.request(op, key, inputs)

    def observe(self, p, condition="merged"):
        return self.call(self.request("observe", "c", dict(subject=self.text(A.prepared_subject(p)), revision=self.text(p["commit"]), condition=self.text(condition)), "observe"))

    def publish(self, p):
        self.assertEqual(self.call(self.action(p), "reconcile")["outcome"], "absent")
        self.assertEqual(self.call(self.action(p))["outcome"], "done")
        self.assertEqual(self.service.posts, 1)

    def merge(self):
        self.service.prs[0].update(state="closed", merged=True, merged_at="2026-11-06T00:00:00Z", merge_commit_sha="1"*40)

    def test_production_publication_recovery_and_real_merge_cleanup(self):
        p = self.prepare()
        self.publish(p)
        self.assertFalse(self.observe(p)["observation"]["satisfied"])
        self.assertTrue(self.observe(p, "checks_passed")["observation"]["satisfied"])
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "cleanup_workspace_v1", "d"))
        self.merge()
        self.assertTrue(self.observe(p)["observation"]["satisfied"])
        clean = self.action(p, "cleanup_workspace_v1", "d")
        self.assertEqual(self.call(clean)["outcome"], "done")
        self.assertFalse(Path(p["worktree"]).exists())
        self.assertEqual(self.call(clean, "reconcile")["outcome"], "done")
        Path(p["worktree"]).mkdir()
        with self.assertRaises(A.Refused):
            self.call(clean, "reconcile")
        self.assertTrue(Path(p["worktree"]).is_dir())

    def test_production_uncertain_post_never_duplicates(self):
        p = self.prepare()
        self.service.crash_post = True
        with self.assertRaises(RuntimeError):
            self.call(self.action(p))
        self.assertEqual(self.call(self.action(p), "reconcile")["outcome"], "done")
        self.assertEqual(self.service.posts, 1)
        self.assertEqual(self.service.pushes, 1)

    def test_production_unknown_post_remains_pending_after_empty_listing(self):
        p = self.prepare()
        self.service.lose_post = True
        with self.assertRaises(RuntimeError):
            self.call(self.action(p))
        for method in ("reconcile", "execute", "reconcile"):
            self.assertEqual(self.call(self.action(p), method)["outcome"], "pending")
        self.assertEqual(self.service.posts, 1)

    def test_changed_head_invalidates_checks_and_merge(self):
        p = self.prepare()
        self.publish(p)
        self.service.prs[0]["head"]["sha"] = "f" * 40
        self.merge()
        for condition in ("checks_passed", "merged"):
            with self.assertRaises(A.Refused):
                self.observe(p, condition)
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "cleanup_workspace_v1", "d"))
        self.assertTrue(Path(p["worktree"]).exists())

    def test_cleanup_retains_dirty_replaced_and_foreign_objects(self):
        p = self.prepare()
        self.publish(p)
        self.merge()
        clean = self.action(p, "cleanup_workspace_v1", "d")
        foreign = Path(p["worktree"]) / "foreign-empty"
        foreign.mkdir()
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertTrue(foreign.is_dir())
        foreign.rmdir()  # Remove only the replacement this test created.
        dirty = Path(p["worktree"]) / "new"
        dirty.write_text("dirty")
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertTrue(dirty.exists())
        dirty.write_text("candidate\n")
        build = Path(p["worktree"]) / "target"
        build.rename(build.with_name("foreign"))
        build.mkdir(mode=0o700)
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertTrue(build.exists())

    def test_raw_verification_handles_a_large_tracked_tree_and_its_final_paths(self):
        self.git("checkout", "-q", "af-candidate")
        directory = self.repo / "many"
        directory.mkdir()
        for index in range(300):
            (directory / str(index).zfill(3)).write_bytes(b"entry\n")
        self.git("add", "many")
        self.git("commit", "-qm", "larger candidate")
        tree = self.git("rev-parse", "HEAD^{tree}")
        transport = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", self.config["source_commit"])
        def native(adapter, task, owner, previous=None):
            return dict(schema="af/git-candidate-source@1", source_snapshot=S, predecessor_snapshot=None,
                        source_commit=self.config["source_commit"], candidate_tree=tree, transport_commit=transport,
                        repository=str(self.repo), candidate_snapshot=R)
        with patch.object(LocalAdapter, "native_source", native):
            p = self.prepare()
        self.publish(p)
        self.merge()
        root = Path(p["worktree"])
        self.adapter().git("update-index", "--assume-unchanged", "many/299", cwd=root)
        (root / "many/299").write_bytes(b"other\n")
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "cleanup_workspace_v1", "d"))
        self.assertEqual((root / "many/299").read_bytes(), b"other\n")

    def test_index_flags_and_disabled_filemode_cannot_hide_dirty_workspace(self):
        p = self.prepare()
        self.publish(p)
        self.merge()
        root = Path(p["worktree"])
        path = root / "new"
        self.git("config", "core.filemode", "false")
        path.chmod(0o644)
        self.assertEqual(self.adapter().git("status", "--porcelain=v1", cwd=root), b"")
        clean = self.action(p, "cleanup_workspace_v1", "d")
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertEqual(path.stat().st_mode & 0o111, 0)
        path.chmod(0o755)
        self.adapter().git("update-index", "--assume-unchanged", "new", cwd=root)
        path.write_bytes(b"operator!\n")
        self.assertEqual(self.adapter().git("status", "--porcelain=v1", cwd=root), b"")
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertEqual(path.read_bytes(), b"operator!\n")

    def test_real_bare_remote_push_and_delete_lease_preserve_a_racing_ref(self):
        bare = self.root / "remote.git"
        self.git("init", "--bare", "-q", str(bare))
        self.git("-c", "protocol.file.allow=always", "push", "--quiet", str(bare),
                 self.config["source_commit"] + ":refs/heads/main")
        test = self
        url = "ssh://git@github.com/" + self.config["repository"] + ".git"
        def remote_tip(branch):
            return subprocess.check_output([test.config["git"], "--git-dir", str(bare), "for-each-ref",
                "--format=%(objectname)", "refs/heads/" + branch], env=test.env).decode().strip() or None
        class RealRemote(A.WorkspaceAdapter):
            race = False
            def git(adapter, *args, **kwargs):
                push = args[0] == "push"
                if push and args[-1].startswith(":") and adapter.race:
                    adapter.race = False
                    # Change the real ref AFTER preflight, before the leased deletion reaches Git.
                    branch = args[-1].removeprefix(":refs/heads/")
                    subprocess.run([test.config["git"], "--git-dir", str(bare), "update-ref",
                        "refs/heads/" + branch, test.config["source_commit"]], env=test.env, check=True)
                if url in args:
                    rewritten = tuple(str(bare) if value == url else value for value in args)
                    result = super().git("-c", "protocol.file.allow=always", *rewritten, **kwargs)
                else:
                    result = super().git(*args, **kwargs)
                if push:
                    test.service.pushes += 1
                    branch = args[-1].split(":refs/heads/", 1)[1]
                    tip = remote_tip(branch)
                    if tip is not None:
                        test.service.refs[branch] = tip
                        for pr in test.service.prs:
                            if pr["head"]["ref"] == branch:
                                pr["head"]["sha"] = tip
                    else:
                        test.service.refs.pop(branch, None)
                return result
        adapter = RealRemote(self.config, self.config_digest, self.service)
        self.adapter = lambda: adapter
        p = self.prepare()
        self.publish(p)
        self.assertEqual(remote_tip(p["branch"]), p["commit"])
        self.merge()
        adapter.race = True
        clean = self.action(p, "cleanup_workspace_v1", "d")
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertEqual(remote_tip(p["branch"]), self.config["source_commit"])
        with self.assertRaises(A.Refused):
            self.call(clean, "reconcile")
        self.assertEqual(remote_tip(p["branch"]), self.config["source_commit"])
        self.assertEqual(remote_tip("main"), self.config["source_commit"])
        self.assertEqual(self.service.posts, 1)

    def test_owned_build_cleanup_does_not_require_project_ignore_rules(self):
        self.git("checkout", "-q", "af-candidate")
        (self.repo / ".gitignore").write_text("foreign/\n")
        self.git("add", ".gitignore")
        self.git("commit", "-qm", "candidate without build ignores")
        tree = self.git("rev-parse", "HEAD^{tree}")
        transport = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", self.config["source_commit"])
        def native(adapter, task, owner, previous=None):
            return dict(schema="af/git-candidate-source@1", source_snapshot=S, predecessor_snapshot=None,
                        source_commit=self.config["source_commit"], candidate_tree=tree, transport_commit=transport,
                        repository=str(self.repo), candidate_snapshot=R)
        with patch.object(LocalAdapter, "native_source", native):
            p = self.prepare()
        self.publish(p)
        self.merge()
        (Path(p["worktree"]) / "target" / "compiled").write_bytes(b"owned build output")
        self.assertEqual(self.call(self.action(p, "cleanup_workspace_v1", "d"))["outcome"], "done")
        self.assertFalse(Path(p["worktree"]).exists())

    def test_exclusive_writer_violation_during_post_never_settles_or_reposts(self):
        p = self.prepare()
        original = self.service.request
        def changed(method, path, body=None):
            response = original(method, path, body)
            if method == "POST":
                self.service.refs[p["branch"]] = "f" * 40
                self.service.prs[0]["head"]["sha"] = "f" * 40
                response["head"]["sha"] = "f" * 40
            return response
        with patch.object(self.service, "request", changed):
            with self.assertRaises(A.Refused):
                self.call(self.action(p))
        for method in ("execute", "reconcile"):
            with self.assertRaises(A.Refused):
                self.call(self.action(p), method)
        self.assertEqual(self.service.posts, 1)
        self.assertTrue(Path(p["worktree"]).exists())

    def test_lost_continuation_push_recovers_same_pr_without_post(self):
        p = self.prepare()
        self.publish(p)
        p = self.repair(p)
        request = self.action(p, key="8")
        original = LocalAdapter.git
        def lost(adapter, *args, **kwargs):
            result = original(adapter, *args, **kwargs)
            if args[0] == "push":
                raise RuntimeError("lost owned push response")
            return result
        with patch.object(LocalAdapter, "git", lost):
            with self.assertRaises(RuntimeError):
                self.call(request)
        self.assertEqual(self.call(request, "reconcile")["outcome"], "done")
        self.assertEqual(self.service.posts, 1)
        self.assertEqual(self.service.prs[0]["head"]["sha"], p["commit"])

    def test_cleanup_retains_replaced_git_metadata_and_changed_refs(self):
        p = self.prepare()
        self.publish(p)
        self.merge()
        clean = self.action(p, "cleanup_workspace_v1", "d")
        git_file = Path(p["worktree"]) / ".git"
        original = git_file.read_bytes()
        foreign = self.root / "foreign-git-file"
        foreign.write_bytes(original)
        git_file.unlink()
        git_file.symlink_to(foreign)
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertTrue(git_file.is_symlink())
        git_file.unlink()
        git_file.write_bytes(original)
        self.service.refs[p["branch"]] = "f" * 40
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertTrue(Path(p["worktree"]).exists())
        self.service.refs[p["branch"]] = p["commit"]
        self.git("update-ref", "refs/heads/" + p["branch"], self.config["source_commit"], p["commit"])
        with self.assertRaises(A.Refused):
            self.call(clean)
        self.assertTrue(Path(p["worktree"]).exists())

    def test_explicit_authority_and_signed_exact_publication(self):
        with self.assertRaises(A.Refused):
            A.workspace_validate_config({**self.config, "state_dir": str(Path(self.config["task_store"]) / "nested-adapter")})
        for mode in (None, "locked_head", ""):
            with self.assertRaises(A.Refused):
                A.workspace_validate_config({**self.config, "publication_mode": mode})
        p = self.prepare()
        r = self.action(p)
        del r["inputs"]["decision"]
        with self.assertRaises(A.Refused):
            self.call(r)
        self.assertEqual(self.service.pushes, 0)
        self.service.refs[p["branch"]] = "f" * 40
        with self.assertRaises(A.Refused):
            self.call(self.action(p))
        self.assertEqual(self.service.posts, 0)

    def test_native_population_bypasses_checkout_attribute_conversion(self):
        # Add eol attributes to the exact candidate object; raw materializer must retain LF.
        self.git("checkout", "-q", "af-candidate")
        (self.repo / ".gitattributes").write_text("new text eol=crlf\n")
        self.git("add", ".gitattributes")
        self.git("commit", "-qm", "candidate attributes")
        tree = self.git("rev-parse", "HEAD^{tree}")
        transport = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", self.config["source_commit"])
        def native(adapter, task, owner, previous=None):
            return dict(schema="af/git-candidate-source@1", source_snapshot=S, predecessor_snapshot=None,
                        source_commit=self.config["source_commit"], candidate_tree=tree, transport_commit=transport,
                        repository=str(self.repo), candidate_snapshot=R)
        with patch.object(LocalAdapter, "native_source", native):
            p = self.prepare()
        self.assertEqual((Path(p["worktree"]) / "new").read_bytes(), b"candidate\n")
        self.publish(p)
        self.merge()
        path = Path(p["worktree"]) / "new"
        self.adapter().git("update-index", "--assume-unchanged", "new", cwd=Path(p["worktree"]))
        path.write_bytes(b"candidate\r\n")
        self.assertEqual(self.adapter().git("status", "--porcelain=v1", cwd=Path(p["worktree"])), b"")
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "cleanup_workspace_v1", "d"))
        self.assertEqual(path.read_bytes(), b"candidate\r\n")


    def test_independent_schemas_match_the_installed_writer(self):
        for name, value in A.workspace_schema_documents().items():
            self.assertEqual(json.loads((ROOT / "schemas" / name).read_text()), value)
        old = copy.deepcopy(self.prepare_req)
        old["schema"] = "af/scenario-adapter-request@3"
        with self.assertRaises(A.Refused):
            self.call(old)
        old_config = dict(self.config, schema="af/git-workspace-config@1")
        with self.assertRaises(A.Refused):
            A.workspace_validate_config(old_config)
        state = Path(self.config["state_dir"]) / "state.json"
        raw = A.decode(state.read_bytes(), A.STATE_LIMIT)
        raw["checksum"] = D
        state.write_bytes(A.canonical(raw))
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req)

    def test_installed_wire_pins_complete_code_and_config(self):
        self.assertIn(b"class Adapter:", self.executable.read_bytes())
        self.assertIn(b"class WorkspaceAdapter:", self.executable.read_bytes())
        out = subprocess.run([str(self.executable)], input=A.canonical(self.prepare_req), capture_output=True,
            env=dict(HOME=str(self.home), PYTHONPATH="/foreign", HTTPS_PROXY="https://foreign.invalid"))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(json.loads(out.stdout)["outcome"], "done")
        self.config_path.write_bytes(A.canonical({**self.config, "base":"other"}))
        out = subprocess.run([str(self.executable)], input=A.canonical(self.prepare_req), capture_output=True, env={})
        self.assertEqual(out.returncode, 1)
        self.assertNotIn(b"TEST_CREDENTIAL", out.stdout + out.stderr)


    def repair(self, previous, key="e"):
        r = copy.deepcopy(self.prepare_req)
        r["intent_id"] = "sha256:" + key * 64
        candidate = r["inputs"]["candidate"]["value"]
        candidate["task_id"] = "repair-" + key
        candidate["result_id"] = "sha256:" + key * 64
        candidate["result"]["outputs"]["snapshot"]["snapshot_id"] = "sha256:" + ("1" if key == "e" else "2") * 64
        r["inputs"]["subject"] = self.text(candidate["result_id"])
        r["inputs"]["revision"] = self.text(candidate["result"]["outputs"]["snapshot"]["snapshot_id"])
        r["inputs"]["previous"] = self.action(previous, "cleanup_workspace_v1")["inputs"]["prepared"]
        # Native accepted-source transport is the injected boundary here; real Git still
        # verifies the parent and raw candidate tree, then prepares an exact signed commit.
        def native(adapter, task, owner, parent):
            self.assertEqual(parent, previous)
            return dict(schema="af/git-candidate-source@1", source_snapshot=A.digest(("rerooted-" + task["task_id"]).encode()), predecessor_snapshot=parent["snapshot_id"],
                        source_commit=parent["commit"], candidate_tree=parent["tree"],
                        transport_commit=parent["commit"], repository=str(self.repo),
                        candidate_snapshot=task["result"]["outputs"]["snapshot"]["snapshot_id"])
        # The fixed native transport branch names a complete candidate tree in this local fixture.
        self.git("update-ref", "refs/heads/af-candidate", previous["commit"])
        with patch.object(LocalAdapter, "native_source", native):
            result = self.call(r)
        self.assertEqual(result["outcome"], "done")
        prepared = json.loads(result["evidence"])
        self.service.source = prepared
        return prepared

    def test_two_continuations_keep_one_pr_and_invalidate_prior_heads(self):
        p = self.prepare()
        self.publish(p)
        for key in ("e", "f"):
            prior = p
            p = self.repair(prior, key)
            request = self.action(p, key=("8" if key == "e" else "9"))
            # Preparation creates only a local commit. Remote stays at the last signed head.
            self.assertEqual(self.service.refs[p["branch"]], prior["commit"])
            self.assertEqual(self.call(request)["outcome"], "done")
            self.assertEqual(self.call(request, "reconcile")["outcome"], "done")
            self.assertEqual(self.service.posts, 1)
            self.assertEqual(self.service.prs[0]["head"]["sha"], p["commit"])
            with self.assertRaises(A.Refused):
                self.observe(prior, "checks_passed")
        self.merge()
        self.assertTrue(self.observe(p)["observation"]["satisfied"])
        self.assertEqual(self.call(self.action(p, "cleanup_workspace_v1", "d"))["outcome"], "done")
        self.assertFalse(self.service.refs)
        self.assertEqual(len(list(Path(self.config["worktree_root"]).iterdir())), 0)

    def test_lost_cleanup_response_recovers_without_deleting_replacement_paths(self):
        p = self.prepare()
        self.publish(p)
        self.merge()
        request = self.action(p, "cleanup_workspace_v1", "d")
        original = LocalAdapter.git
        def crash(adapter, *args, **kwargs):
            result = original(adapter, *args, **kwargs)
            if args[:2] == ("worktree", "remove"):
                raise RuntimeError("lost cleanup response")
            return result
        with patch.object(LocalAdapter, "git", crash):
            with self.assertRaises(RuntimeError):
                self.call(request)
        path = Path(p["worktree"])
        self.assertFalse(path.exists())
        path.mkdir()
        (path / "foreign").write_text("preserve")
        with self.assertRaises(A.Refused):
            self.call(request, "reconcile")
        self.assertEqual((path / "foreign").read_text(), "preserve")
        # Removing this test-owned replacement permits the original cleanup intent to reconcile.
        shutil.rmtree(path)
        self.assertEqual(self.call(request, "reconcile")["outcome"], "done")

    def deletion_crash(self, kind, when, replacement):
        p = self.prepare()
        self.publish(p)
        self.merge()
        request = self.action(p, "cleanup_workspace_v1", "d")
        original = LocalAdapter.git
        def crash(adapter, *args, **kwargs):
            deleting = (args[:2] == ("update-ref", "-d")) if kind == "branch" else args[0] == "push"
            if deleting and when == "before":
                raise RuntimeError("deletion interrupted")
            result = original(adapter, *args, **kwargs)
            if deleting:
                raise RuntimeError("deletion interrupted")
            return result
        with patch.object(LocalAdapter, "git", crash):
            with self.assertRaisesRegex(RuntimeError, "deletion interrupted"):
                self.call(request)
        ref = "refs/heads/" + p["branch"]
        if kind == "branch":
            self.git("update-ref", "-d", ref)
        else:
            self.service.refs.pop(p["branch"], None)
        if replacement != "absent":
            sha = self.config["source_commit"] if replacement == "changed" else p["commit"]
            if kind == "branch":
                self.git("update-ref", ref, sha)
            else:
                self.service.refs[p["branch"]] = sha
            with self.assertRaises(A.Refused):
                self.call(request, "reconcile")
            actual = self.git("rev-parse", ref) if kind == "branch" else self.service.refs[p["branch"]]
            self.assertEqual(actual, sha)
        else:
            self.assertEqual(self.call(request, "reconcile")["outcome"], "done")

    def test_completed_checks_route_failure_and_preserve_pending_and_stale(self):
        p = self.prepare()
        self.publish(p)
        ready = self.observe(p, "checks_completed")["observation"]
        self.assertTrue(ready["satisfied"])
        self.assertEqual(json.loads(ready["evidence"])["state"], "ready")
        self.service.green = False
        failed = self.observe(p, "checks_completed")["observation"]
        self.assertTrue(failed["satisfied"])
        packet = json.loads(failed["evidence"])
        self.assertEqual(packet["state"], "repairable")
        self.assertEqual(packet["revision"], p["commit"])
        self.assertEqual(packet["checks"][0]["identity"], self.config["required_checks"][0])
        self.assertFalse(self.observe(p, "checks_passed")["observation"]["satisfied"])
        pages = self.service.pages
        def pending(path, **kwargs):
            rows = pages(path, **kwargs)
            if path.endswith("/check-runs"):
                rows[0].update(status="in_progress", conclusion=None)
            return rows
        with patch.object(self.service, "pages", pending):
            observation = self.observe(p, "checks_completed")["observation"]
            self.assertFalse(observation["satisfied"])
            self.assertEqual(json.loads(observation["evidence"])["state"], "pending")
        def stale(path, **kwargs):
            rows = pages(path, **kwargs)
            if path.endswith("/check-runs"):
                rows[0]["head_sha"] = "0" * 40
            return rows
        with patch.object(self.service, "pages", stale):
            with self.assertRaises(A.Refused):
                self.observe(p, "checks_completed")
        repaired = self.repair(p)
        self.assertEqual(self.call(self.action(repaired, key="8"))["outcome"], "done")
        self.assertEqual(self.service.posts, 1)
        with self.assertRaises(A.Refused):
            self.observe(p, "checks_completed")
        self.service.green = True
        self.assertEqual(json.loads(self.observe(repaired, "checks_completed")["observation"]["evidence"])["state"], "ready")

for _ref in ("branch", "remote"):
    for _when in ("before", "after"):
        for _replacement in ("absent", "unchanged", "changed", "same_sha_recreated"):
            def _deletion(self, kind=_ref, when=_when, replacement=_replacement):
                self.deletion_crash(kind, when, replacement)
            setattr(LifecycleTests, "test_delete_" + _ref + "_" + _when + "_" + _replacement, _deletion)


# Every boundary runs with a fresh machine journal and real local Git. Restart drops
# all in-memory state, including mutations made before an interrupted journal save.
for _kind in ("worktree", "build"):
    for _boundary in ("create", "sync_stage", "sync_root", "ownership", "publish", "sync_final", "completion"):
        for _when in ("before", "after"):
            def _case(self, kind=_kind, event=_boundary + "." + _when):
                self.reservation_crash(kind, event)
            setattr(LifecycleTests, "test_reservation_" + _kind + "_" + _boundary + "_" + _when, _case)

for _kind in ("worktree", "build"):
    for _event in ("ownership.after", "publish.after", "completion.after"):
        def _changed(self, kind=_kind, event=_event):
            self.changed_reservation(kind, event)
        setattr(LifecycleTests, "test_changed_reservation_" + _kind + "_" + _event.replace(".", "_"), _changed)

for _kind in ("worktree", "build"):
    for _when in ("before", "after"):
        def _slot(self, kind=_kind, when=_when):
            self.uncertain_slot_crash(kind, when)
        setattr(LifecycleTests, "test_slot_advancement_" + _kind + "_" + _when, _slot)
    def _foreign(self, kind=_kind):
        self.foreign_destination(kind)
    setattr(LifecycleTests, "test_foreign_final_" + _kind, _foreign)
    def _symlink(self, kind=_kind):
        self.uncertain_symlink(kind)
    setattr(LifecycleTests, "test_uncertain_symlink_" + _kind, _symlink)


def performance_integration(native=None):
    """Installed code + native CLI handoff + a real Git remote and controlled HTTP service.

    The Rust fixture supplies accepted real Task receipts. The standalone script uses the
    existing fake native CLI seam; neither variant runs a benchmark or contacts GitHub.
    """
    import http.client
    import http.server
    import threading
    import types
    from urllib.parse import urlsplit
    test = LifecycleTests(methodName="test_independent_schemas_match_the_installed_writer")
    test.setUp()
    try:
        c = copy.deepcopy(test.config)
        c.update(state_dir=str(test.root / "performance-state"), branch_prefix="af-performance",
                 operations=[A.PERFORMANCE_OP])
        if native is not None:
            c.update(repo=native["repo"], task_store=native["task_state"], af=native["af"],
                     source_commit=native["source_commit"], source_content_digest=native["source_content_digest"])
            c["af_digest"] = A.file_digest(c["af"])
            proof_request = native["request"]
            candidate, verification, report, observation = (proof_request[k] for k in ("candidate", "verification", "report", "evidence"))
            c["performance"] = {k: native["lab"][k] for k in ("executable", "digest")}
            c["performance"]["destination"] = "native:claw"
        else:
            candidate = test.candidate
            verification = dict(candidate, task_id="verification", result_id=A.digest(b"verification"))
            report = dict(candidate, task_id="report", result_id=A.digest(b"report"))
            completion = json.loads((ROOT / "fixtures/scenarios/performance-positive-completion.json").read_text())
            completion.update(baseline_snapshot_id=S, candidate_snapshot_id=R)
            observation = dict(subject=D, revision=R, condition="native_verified", satisfied=True,
                               evidence=A.canonical(completion).decode())
            # Fake only the native read-only proof receipt, just as candidate-source in the
            # existing lifecycle fixture. The Rust caller uses the real pinned native CLI.
            binary = Path(c["af"])
            source = binary.read_text()
            marker = 'assert args[:2] == ["scenario", "candidate-source"]'
            proof_body = "Accepted final report\n<!-- af-task-report:v1 -->\n" + " ".join(t["task_id"] for t in (candidate, verification, report)) + "\n<!-- /af-task-report -->\n" + observation["evidence"]
            source = source.replace(marker, '''if args[:2] == ["scenario", "performance-proof"]:
 request=json.loads(pathlib.Path(args[args.index("--file")+1]).read_text())
 assert request['source_commit']==''' + repr(c["source_commit"]) + '''
 assert request['source_content_digest']==''' + repr(c["source_content_digest"]) + '''
 print(json.dumps(dict(schema='af/performance-pr-proof@1',request_digest='sha256:'+hashlib.sha256(json.dumps(request,sort_keys=True,separators=(',', ':'),ensure_ascii=False).encode()).hexdigest(),source_snapshot=''' + repr(S) + ''',candidate_snapshot=request['candidate']['result']['outputs']['snapshot']['snapshot_id'],body=''' + repr(proof_body) + ''')))
 sys.exit(0)
''' + marker)
            binary.write_text(source)
            c["af_digest"] = A.file_digest(binary)
            lab = test.root / "lab"
            lab.write_text(fixture_launcher("import json,sys\nr=json.load(sys.stdin)\nprint(json.dumps(dict(schema='af/scenario-adapter-response@1',intent_id=r['intent_id'],input_digest=r['input_digest'],outcome='done',evidence='native fixture',observation=" + repr(observation) + ")))\n"))
            lab.chmod(0o700)
            c["performance"] = dict(executable=str(lab), digest=A.file_digest(lab), destination="native:claw")
        if native is None:
            test.git("branch", "-M", "main")
        remote = test.root / "performance.git"
        subprocess.run([c["git"], "init", "--bare", "-q", str(remote)], env=test.env, check=True)
        subprocess.run([c["git"], "-C", c["repo"], "push", "-q", str(remote), c["source_commit"] + ":refs/heads/main"], env=test.env, check=True)
        git = c["git"]
        calls = test.root / "pushes"
        wrapper = test.root / "performance-git"
        wrapper.write_text(performance_git_launcher(git, '''import os,pathlib,subprocess,sys
args=sys.argv[1:]
url='ssh://git@github.com/octo/project.git'
if url in args:
 assert 'push' in args or 'ls-remote' in args
 args=[''' + repr(str(remote)) + ''' if v==url else 'protocol.file.allow=always' if v=='protocol.file.allow=never' else v for v in args]
if 'commit-tree' in args and ''' + repr(native is not None) + ''':
 result=subprocess.run([''' + repr(git) + ''']+args,env=os.environ,capture_output=True)
 if result.returncode: sys.exit(result.returncode)
 marker=pathlib.Path(''' + repr(str(test.root / "commits")) + ''')
 with marker.open('a') as out: out.write(result.stdout.decode())
 if len(marker.read_text().splitlines())==1: sys.exit(2)
 sys.stdout.buffer.write(result.stdout)
 sys.exit(0)
if 'push' in args:
 assert '--force' not in args
 assert any(v.startswith('--force-with-lease=refs/heads/af-performance/') and v.endswith(':') for v in args)
 with open(''' + repr(str(calls)) + ''','a') as out: out.write('push\\n')
 result=subprocess.run([''' + repr(git) + ''']+args,env=os.environ)
 if result.returncode: sys.exit(result.returncode)
 if pathlib.Path(''' + repr(str(calls)) + ''').read_text().count('push')==1: sys.exit(2) # response lost after real push
 sys.exit(0)
os.execve(''' + repr(git) + ''',[''' + repr(git) + ''']+args,os.environ)
'''))
        wrapper.chmod(0o700)
        c.update(git=str(wrapper), git_digest=A.file_digest(wrapper))
        absent_scope = dict(c)
        del absent_scope["performance"]
        try:
            A.workspace_validate_config(absent_scope)
            raise AssertionError("standing operation installed without native evidence scope")
        except A.Refused:
            pass
        config = test.root / "performance-config.json"
        config.write_bytes(A.canonical(c))
        config.chmod(0o600)
        installed_path = test.root / "performance-adapter"
        with contextlib.redirect_stdout(io.StringIO()):
            I.install(config, installed_path)
        installed = types.ModuleType("installed_performance")
        exec(compile(installed_path.read_text(), str(installed_path), "exec"), installed.__dict__)
        # Pins and config validation execute on the installed bytes, including native tools.
        installed.workspace_load_config()
        scope = dict(repository=c["repository"], repository_id=c["repository_id"], repo=c["repo"], task_state=c["task_store"],
            base=c["base"], branch_prefix=c["branch_prefix"], source_commit=c["source_commit"], source_content_digest=c["source_content_digest"],
            evidence_adapter="lab", evidence_destination="native:claw")
        req = test.request(A.PERFORMANCE_OP, "b", dict(subject=test.text(candidate["result_id"]),
            revision=test.text(candidate["result"]["outputs"]["snapshot"]["snapshot_id"]),
            body=test.text(A.canonical(dict(schema="af/performance-pr-content@1", title="Improve measured performance")).decode()),
            candidate=dict(type="task", value=candidate), verification=dict(type="task", value=verification),
            report=dict(type="task", value=report), measurement=dict(type="observation", value=observation), machine_scope=test.text(A.canonical(scope).decode())))
        # Strict installed request scope fails before native export, commit or publication.
        for field in ["repository_id", "base", "branch_prefix", "source_commit", "repo", "task_state"]:
            wrong = copy.deepcopy(req)
            changed = dict(scope)
            changed[field] = 99 if field == "repository_id" else "/foreign" if field in ("repo", "task_state") else "foreign"
            wrong["inputs"]["machine_scope"] = test.text(A.canonical(changed).decode())
            try:
                installed.workspace_validate_request(wrong, c)
                raise AssertionError("accepted wrong machine " + field)
            except installed.Refused:
                pass
        wrong = copy.deepcopy(req)
        del wrong["inputs"]["machine_scope"]
        try:
            installed.workspace_validate_request(wrong, c)
            raise AssertionError("accepted absent standing scope")
        except installed.Refused:
            pass
        state = {"prs": [], "posts": 0, "lose_post": True, "base": c["source_commit"]}
        def tip(branch):
            return subprocess.check_output([git, "--git-dir", str(remote), "rev-parse", "refs/heads/" + branch], env=test.env).decode().strip()
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass
            def do_GET(self):
                path = urlsplit(self.path).path
                if path == "/user": data = dict(id=7, login="owner")
                elif path == "/repos/octo/project": data = dict(id=17, full_name="octo/project")
                elif path == "/repos/octo/project/git/ref/heads/main": data = dict(ref="refs/heads/main", object=dict(type="commit", sha=state["base"]))
                elif path == "/repos/octo/project/pulls": data = state["prs"]
                else:
                    assert path == "/repos/octo/project/pulls/1", path
                    data = state["prs"][0]
                self.send_response(200)
                self.end_headers()
                self.wfile.write(A.canonical(data))
            def do_POST(self):
                assert self.path == "/repos/octo/project/pulls"
                data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                assert data["draft"] is False and data["maintainer_can_modify"] is False
                state["posts"] += 1
                assert state["posts"] == 1
                p = dict(id=101, number=1, user=dict(id=7, login="owner"), title=data["title"], body=data["body"],
                    url="https://api.github.com/repos/octo/project/pulls/1", html_url="https://github.com/octo/project/pull/1",
                    head=dict(ref=data["head"], sha=tip(data["head"]), repo=dict(id=17, full_name="octo/project")),
                    base=dict(ref=data["base"], repo=dict(id=17, full_name="octo/project")), state="open", merged=False)
                state["prs"].append(p)
                if state["lose_post"]:
                    self.close_connection = True
                    return
                self.send_response(201)
                self.end_headers()
                self.wfile.write(A.canonical(p))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        class HTTP:
            def request(self, method, path, body=None):
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=5)
                try:
                    connection.request(method, path, body=None if body is None else A.canonical(body), headers={"Content-Type": "application/json"})
                    response = connection.getresponse()
                    assert response.status in (200, 201)
                    return json.loads(response.read())
                finally:
                    connection.close()
            def pages(self, path, key=None, params=None):
                assert key is None
                return self.request("GET", path)
        service = HTTP()
        adapter = installed.WorkspaceAdapter(c, A.digest(config.read_bytes()), service)
        original_git = adapter.git
        commits = []
        def lost_commit(*args, **kwargs):
            result = original_git(*args, **kwargs)
            if args[0] == "commit-tree":
                commits.append(result)
                if len(commits) == 1:
                    raise RuntimeError("lost commit response")
            return result
        adapter.git = lost_commit
        try:
            if native is None:
                # Refuse stale base and forged supplied evidence before the first commit/push.
                state["base"] = "f" * 40
                try:
                    adapter.handle(req)
                    raise AssertionError("accepted stale base before commit")
                except installed.Refused:
                    pass
                state["base"] = c["source_commit"]
                forged = copy.deepcopy(req)
                altered = json.loads(forged["inputs"]["measurement"]["value"]["evidence"])
                altered["result_sha256"] = S
                forged["inputs"]["measurement"]["value"]["evidence"] = A.canonical(altered).decode()
                # A distinct intent isolates the retained failed proof request.
                forged["intent_id"] = "sha256:" + "c" * 64
                forged["inputs"]["subject"] = test.text(A.digest(b"other result"))
                forged["inputs"]["candidate"]["value"]["result_id"] = A.digest(b"other result")
                other_snapshot = A.digest(b"other candidate")
                forged["inputs"]["revision"] = test.text(other_snapshot)
                forged["inputs"]["candidate"]["value"]["result"]["outputs"]["snapshot"]["snapshot_id"] = other_snapshot
                forged["inputs"]["measurement"]["value"]["revision"] = other_snapshot
                altered["candidate_snapshot_id"] = other_snapshot
                forged["inputs"]["measurement"]["value"]["evidence"] = A.canonical(altered).decode()

                try:
                    adapter.handle(forged)
                    raise AssertionError("accepted supplied evidence differing from native lab")
                except installed.Refused as error:
                    assert "measurement differs from retained native lab evidence" in str(error), str(error)
                duplicate = copy.deepcopy(req)
                duplicate["intent_id"] = "sha256:" + "d" * 64
                duplicate["inputs"]["subject"] = test.text(A.digest(b"duplicate result"))
                duplicate["inputs"]["candidate"]["value"]["result_id"] = A.digest(b"duplicate result")
                try:
                    adapter.handle(duplicate)
                    raise AssertionError("same candidate admitted under a different intent/result")
                except installed.Refused as error:
                    assert "candidate Snapshot already reserved" in str(error), str(error)
                assert commits == [] and not calls.exists() and state["posts"] == 0
            if native is not None:
                # Pin the installed application with only its HTTPS connection mapped to the
                # controlled HTTP server. Production has no configurable endpoint or proxy.
                controlled = test.root / "controlled-installed-adapter"
                controlled.write_text(fixture_launcher("import http.client\ndef _fixture_connection(host, timeout, context):\n assert host == 'api.github.com'\n return http.client.HTTPConnection('127.0.0.1', " + str(server.server_port) + ", timeout=timeout)\nhttp.client.HTTPSConnection = _fixture_connection\n" + installed_path.read_text()))
                controlled.chmod(0o700)
                def literal(value):
                    return dict(source="literal", value=dict(type="text", value=value))
                def ref(name):
                    return dict(source="step", step=name, port="result")
                targets = {t["task_id"]:t for t in native["targets"].values()}
                stages = [dict(id=name, operation=dict(kind="task", target=targets[t["task_id"]]), inputs={},
                    outputs=dict(result="task"), when=None, max_attempts=1, timeout_ms=30000, on_timeout="fail")
                    for name, t in (("candidate",candidate),("verification",verification),("report",report))]
                observed = dict(adapter="lab", operation="observe", destination="native:claw")
                grant = dict(adapter="publication", operation=A.PERFORMANCE_OP, destination=req["destination"])
                stages.append(dict(id="measurement", operation=dict(kind="wait", grant=observed, condition="native_verified"),
                    inputs=dict(subject=literal(observation["subject"]), revision=literal(observation["revision"])),
                    outputs=dict(result="observation"),when=None,max_attempts=1,timeout_ms=30000,on_timeout="fail"))
                stages.append(dict(id="publish", operation=dict(kind="action", grant=grant, requires_decision=False, required_condition=None),
                    inputs=dict(subject=literal(candidate["result_id"]), revision=literal(observation["revision"]), body=literal(req["inputs"]["body"]["value"]),
                        candidate=ref("candidate"),verification=ref("verification"),report=ref("report"),measurement=ref("measurement")),
                    outputs=dict(result="action"),when=None,max_attempts=1,timeout_ms=30000,on_timeout="fail"))
                limits = dict(max_tokens=32000000,max_wall_ms=691200000,max_dispatches=16)
                definition = dict(schema="af/scenario@6",inputs={},outputs=dict(publication="action"),admission=None,steps=stages,
                    result=dict(publication=ref("publish")),completion=[dict(source="literal",value=dict(type="boolean",value=True))],
                    authority=dict(task_plans=sorted({t["plan_id"] for t in (candidate,verification,report)}),task_templates=[],grants=[grant,observed],decision_owners=[]),limits=limits)
                bundle = dict(schema="af/scenario-bundle@6",root="publish",definitions=dict(publish=definition))
                policy = dict(schema="af/scenario-policy@5",adapters=dict(publication=dict(executable=str(controlled),digest=A.file_digest(controlled),
                    operations=[A.PERFORMANCE_OP],destinations=[req["destination"]],max_wall_ms=30000,max_output_bytes=65536),lab=native["lab"]),
                    task_plans=definition["authority"]["task_plans"],task_templates=[],decision_keys={},file_roots=[],performance_publications=dict(publication=scope))
                for name, doc in (("bundle",bundle),("policy",policy),("inputs",{})):
                    path = test.root / (name + ".json")
                    path.write_bytes(A.canonical(doc))
                    path.chmod(0o600)
                env = dict(PATH="/usr/bin:/bin",HOME=c["state_dir"]+"/home",AF_SELF_OFFLINE="1",TMPDIR=c["state_dir"],
                    XDG_DATA_HOME=c["state_dir"]+"/home",XDG_STATE_HOME=c["state_dir"]+"/home",XDG_CONFIG_HOME=c["state_dir"]+"/home",XDG_CACHE_HOME=c["state_dir"]+"/home")
                def cli(*args):
                    out = subprocess.run([c["af"],*args,"--state",str(test.root / "coordinator"),"--json"],env=env,capture_output=True)
                    assert out.returncode == 0, out.stderr.decode()
                    return json.loads(out.stdout)
                validate = cli("scenario","validate","--file",str(test.root / "bundle.json"),"--authority",str(test.root / "policy.json"))
                response = cli("scenario","start","publication","--file",str(test.root / "bundle.json"),"--authority",str(test.root / "policy.json"),
                    "--inputs",str(test.root / "inputs.json"),"--confirm-pin",validate["revision"],"--execute")
                intent = response["waiting"]["intent_id"]
                for _ in range(3):
                    assert response["status"] == "waiting" and response["waiting"]["kind"] == "reconcile", response
                    assert response["waiting"]["intent_id"] == intent
                    response = cli("scenario","resume","publication")
                assert response["status"] == "succeeded", response
                assert cli("scenario","resume","publication") == response
                commits = [line.encode() for line in (test.root / "commits").read_text().splitlines()]
            else:
                assert adapter.handle(dict(req, method="reconcile"))["outcome"] == "absent"

                for expected in ("lost commit response", "bounded native operation failed", "Remote end closed"):
                    try:
                        adapter.handle(req)
                        raise AssertionError("expected ambiguous outcome: " + expected)
                    except (RuntimeError, http.client.RemoteDisconnected) as error:
                        assert expected in str(error), (expected, str(error))
                response = adapter.handle(dict(req, method="reconcile"))
                assert response["outcome"] == "done", response
                assert adapter.handle(req)["outcome"] == "done"
            assert len(commits) == 2 and commits[0] == commits[1]
            assert calls.read_text() == "push\n" and state["posts"] == 1 and len(state["prs"]) == 1
            pr = state["prs"][0]
            assert tip(pr["head"]["ref"]) == pr["head"]["sha"] == commits[0].decode()
            assert tip("main") == c["source_commit"]
            assert pr["body"].count("<!-- af-task-report:v1 -->") == 1
            assert pr["body"].count("<!-- /af-task-report -->") == 1
            assert "improvement" in pr["body"] and "100" in pr["body"] and "80" in pr["body"]
            for receipt in (candidate, verification, report):
                assert receipt["task_id"] in pr["body"]
            if native is None:
                # Current base drift fails even after a successful response, without another effect.
                state["base"] = "f" * 40
                try:
                    adapter.handle(req)
                    raise AssertionError("accepted drifted base")
                except installed.Refused:
                    pass
                assert state["posts"] == 1 and calls.read_text() == "push\n"
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
    finally:
        test.doCleanups()


class PerformancePublicationTests(unittest.TestCase):
    def test_installed_performance_publication_recovers_commit_push_and_http_response_without_duplicates(self):
        performance_integration()

    def test_standing_operation_cannot_be_enabled_in_generic_github_installation(self):
        config = dict(schema=A.CONFIG_SCHEMA, repository="octo/project", repository_id=17, actor=dict(id=7, login="owner"),
            token_file="/private/token", state_dir="/private/state", heads={"head":"main"},
            required_checks=[dict(kind="check", name="CI", app_id=4)], reviewers=[], operations=[A.PERFORMANCE_OP])
        with self.assertRaises(A.Refused):
            A.validate_config(config)

if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--native-performance-fixture":
        performance_integration(json.loads(Path(sys.argv[2]).read_text()))
    else:
        unittest.main()
