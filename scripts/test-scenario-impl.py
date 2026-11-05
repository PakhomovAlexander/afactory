#!/usr/bin/env python3
"""Hermetic lifecycle tests: real Git, fake native source transport and fake GitHub; no publication."""
import copy
import contextlib
import io
import importlib.util
import json
import os
from pathlib import Path
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


I = module("impl_installer", ROOT / "scripts/install-scenario-impl-adapter.py")
A = I.module()
B = module("impl_builder", ROOT / "scripts/scenario-impl.py")
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
            pr = dict(id=100 + self.posts, number=self.posts, user=dict(id=7), title=body["title"], body=body["body"],
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


class LocalAdapter(A.ImplAdapter):
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
        self.git("branch", "af-impl-candidate", transport)
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
        native = dict(schema="af/scenario-impl-source@1", repository_id="fixture", source_snapshot=S,
                      candidate_snapshot=R, predecessor_snapshot=None, source_content_digest=D, source_commit=source_commit,
                      candidate_tree=tree, transport_commit=transport)
        af.write_text('''#!/usr/bin/python3 -IS
import hashlib, json, pathlib, sys
args=sys.argv[1:]
assert args[:2] == ["scenario", "impl-source"]
values=dict(zip(args[2:-1:2],args[3:-1:2]))
assert args[-1] == "--json"
request=json.loads(pathlib.Path(values["--file"]).read_text())
assert request["schema"] == "af/scenario-impl-source-request@1"
source=''' + repr(native) + '''
source["request_digest"]="sha256:"+hashlib.sha256(json.dumps(request,sort_keys=True,separators=(",", ":"),ensure_ascii=False).encode()).hexdigest()
# Reuse the fixture repository's already pinned af-impl-candidate ref; no real af is spawned.
source["repository"]=values["--into"]+"/repository.git"
p=pathlib.Path(values["--into"])
p.mkdir(exist_ok=True)
(p/"repository.git").symlink_to(pathlib.Path(values["--repo"])/".git") if not (p/"repository.git").exists() else None
print(json.dumps(source))
''')
        af.chmod(0o700)
        git_path = Path(shutil.which("git")).resolve()
        self.config = dict(schema=A.IMPL_CONFIG, repository="octo/project", repository_id=17,
            actor=dict(id=7, login="owner"), token_file=str(token), state_dir=str(self.root / "state"),
            repo=str(self.repo), task_store=str(task_store), worktree_root=str(worktrees), base="main",
            branch_prefix="impl", source_commit=source_commit, source_content_digest=D,
            af=str(af), af_digest=A.digest(af.read_bytes()), git=str(git_path), git_digest=A.digest(git_path.read_bytes()),
            ssh_config=str(ssh), ssh_config_digest=A.digest(ssh.read_bytes()), author_name="Owner", author_email="owner@example.invalid",
            required_checks=[dict(kind="check", name="CI", app_id=4)], reviewers=[dict(id=8, login="reviewer")])
        self.config_path = self.root / "config.json"
        self.config_path.write_bytes(A.canonical(self.config))
        self.config_path.chmod(0o600)
        self.config_digest = A.digest(self.config_path.read_bytes())
        A.CONFIG_DIGEST = self.config_digest
        self.executable = self.root / "adapter"
        with contextlib.redirect_stdout(io.StringIO()):
            I.install(self.config_path, self.executable)
        self.service = Service()
        self.candidate = dict(task_id="implementation", plan_id=D, revision_id=S, result_id=D, charged_tokens=1,
            result=dict(task_revision_id=S, execution="completed", acceptance="satisfied", domain_conclusion="verified",
                outputs=dict(snapshot=dict(artifact_type="af/SourceTree@1", cardinality="one", artifact_ids=[D], snapshot_id=R)),
                evidence=[D], missing_obligations=[]))
        self.prepare_req = self.request("impl_prepare_v1", "a", dict(subject=self.text(D), revision=self.text(R),
            body=self.text(A.canonical(dict(schema="af/scenario-impl-publication@1", title="Implement CLI", body="Exact approved body")).decode()),
            candidate=dict(type="task", value=self.candidate)))

    def git(self, *args):
        p = subprocess.run([shutil.which("git"), *args], cwd=self.repo, env=self.env, capture_output=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout.decode().strip()

    @staticmethod
    def text(value):
        return dict(type="text", value=value)

    def request(self, op, key, inputs, method="execute"):
        return dict(schema="af/scenario-adapter-request@3", operation=op, intent_id="sha256:" + key * 64,
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

    def action(self, p, op="impl_publish_v1", key="b"):
        prepared = dict(intent_id=p["owner"], operation="impl_prepare_v1", destination="github:octo/project",
                        subject=p["result_id"], revision=p["snapshot_id"], evidence=A.canonical(p).decode())
        inputs = dict(subject=self.text(A.prepared_subject(p)), revision=self.text(p["commit"]),
                      body=self.text(A.canonical(p).decode()), prepared=dict(type="action", value=prepared))
        if op == "impl_publish_v1":
            inputs["decision"] = dict(type="decision", value=dict(schema="af/scenario-signed-decision@1", signature="kernel-checks-signature",
                request=dict(approve=True, subject=A.prepared_subject(p), revision=p["commit"], expires_unix_ms=9999999999999)))
        else:
            inputs["evidence"] = dict(type="observation", value=dict(subject=A.prepared_subject(p), revision=p["commit"],
                condition="merged", satisfied=True, evidence="native exact-head observation"))
        return self.request(op, key, inputs)

    def observe(self, p, condition="merged"):
        return self.call(self.request("observe", "c", dict(subject=self.text(A.prepared_subject(p)),
            revision=self.text(p["commit"]), condition=self.text(condition)), "observe"))

    def publish(self, p):
        r = self.action(p)
        self.assertEqual(self.call(r, "reconcile")["outcome"], "absent")
        self.assertEqual(self.call(r)["outcome"], "done")
        return r

    def merge(self):
        self.service.prs[0].update(state="closed", merged=True, merged_at="2026-11-05T00:00:00Z", merge_commit_sha="1" * 40)

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

    def test_exact_publication_requires_decision_and_prepared_provenance(self):
        p = self.prepare()
        for mutate in (lambda r: r["inputs"].pop("decision"),
                       lambda r: r["inputs"]["revision"].update(value="0" * 40),
                       lambda r: r["inputs"]["prepared"]["value"].update(intent_id=D),
                       lambda r: r.update(destination="github:foreign/project")):
            r = self.action(p)
            mutate(r)
            with self.assertRaises(A.Refused):
                self.call(r)
        self.assertEqual(self.service.posts, 0)
        self.assertEqual(self.service.pushes, 0)
        with self.assertRaises(A.Refused):
            self.call(self.action(p), "recover")

    def test_crash_after_post_recovers_same_pr_without_another_push_or_post(self):
        p = self.prepare()
        r = self.action(p)
        self.service.crash_post = True
        with self.assertRaises(RuntimeError):
            self.call(r)
        self.service.crash_post = False
        self.assertEqual(self.call(r, "reconcile")["outcome"], "done")
        self.assertEqual(self.call(r, "reconcile")["outcome"], "done")
        self.assertEqual((self.service.posts, self.service.pushes), (1, 1))

    def test_unknown_post_never_becomes_absent_and_never_duplicates_a_pr(self):
        p = self.prepare()
        r = self.action(p)
        self.service.lose_post = True
        with self.assertRaises(RuntimeError):
            self.call(r)
        self.assertEqual(self.call(r, "reconcile")["outcome"], "pending")
        self.assertEqual(self.call(r)["outcome"], "pending")
        self.assertEqual(self.service.posts, 1)

    def test_foreign_remote_branch_is_preserved_without_push_or_pr(self):
        p = self.prepare()
        self.service.refs[p["branch"]] = "0" * 40
        with self.assertRaises(A.Refused):
            self.call(self.action(p))
        self.assertEqual(self.service.pushes, 0)
        self.assertEqual(self.service.posts, 0)

    def test_completion_waits_for_review_and_actual_human_merge(self):
        p = self.prepare()
        self.publish(p)
        self.assertEqual(self.observe(p, "checks_passed")["outcome"], "done")
        self.assertEqual(self.observe(p, "review_passed")["outcome"], "pending")
        self.service.reviews = [dict(id=1, user=dict(id=8), state="APPROVED", commit_id=p["commit"])]
        self.assertEqual(self.observe(p, "review_passed")["outcome"], "done")
        self.assertEqual(self.observe(p)["outcome"], "pending")
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "impl_cleanup_v1", "d"))
        self.merge()
        self.assertEqual(self.observe(p)["outcome"], "done")
        out = self.call(self.action(p, "impl_cleanup_v1", "d"))
        self.assertEqual(out["outcome"], "done")
        self.assertFalse(Path(p["worktree"]).exists())
        self.assertFalse(self.git("for-each-ref", "--format=%(refname)", "refs/heads/" + p["branch"]))
        self.assertNotIn(p["branch"], self.service.refs)
        self.assertEqual(self.call(self.action(p, "impl_cleanup_v1", "d"), "reconcile")["outcome"], "done")
        replacement = Path(p["worktree"])
        replacement.mkdir()
        (replacement / "foreign").write_text("preserve after lost terminal response")
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "impl_cleanup_v1", "d"), "reconcile")
        self.assertTrue((replacement / "foreign").is_file())

    def test_green_outcome_waits_through_human_merge_and_admits_later_review_failure(self):
        p = self.prepare()
        self.publish(p)
        self.service.reviews = [dict(id=1, user=dict(id=8), state="APPROVED", commit_id=p["commit"])]
        self.assertEqual(self.observe(p, "implementation_outcome")["outcome"], "pending")
        self.service.reviews.append(dict(id=2, user=dict(id=8), state="CHANGES_REQUESTED", commit_id=p["commit"], body="Exercise an error case"))
        repair = self.observe(p, "implementation_outcome")
        self.assertTrue(json.loads(repair["evidence"])["repair"])
        self.service.reviews.append(dict(id=3, user=dict(id=8), state="APPROVED", commit_id=p["commit"]))
        self.assertEqual(self.observe(p, "implementation_outcome")["outcome"], "pending")
        self.merge()
        self.assertTrue(json.loads(self.observe(p, "implementation_outcome")["evidence"])["ready"])

    def test_changed_heads_invalidate_checks_review_and_merge(self):
        p = self.prepare()
        self.publish(p)
        self.merge()
        self.service.prs[0]["head"]["sha"] = "0" * 40
        for condition in ("checks_passed", "review_passed", "merged"):
            with self.assertRaises(A.Refused):
                self.observe(p, condition)
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "impl_cleanup_v1", "d"))
        self.assertTrue(Path(p["worktree"]).exists())

    def test_closed_unmerged_reports_outcome_and_preserves_workspace(self):
        p = self.prepare()
        self.publish(p)
        self.service.prs[0]["state"] = "closed"
        self.assertEqual(self.observe(p)["outcome"], "failed")
        self.assertTrue(Path(p["worktree"]).exists())
        self.assertIn(p["branch"], self.service.refs)

    def test_cleanup_preserves_dirty_foreign_and_replaced_build_content(self):
        p = self.prepare()
        self.publish(p)
        self.merge()
        path = Path(p["worktree"])
        (path / "new").write_text("foreign change\n")
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "impl_cleanup_v1", "d"))
        self.assertTrue(path.exists())
        (path / "new").write_text("candidate\n")
        self.git("update-ref", "refs/heads/" + p["branch"], p["source_commit"], p["commit"])
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "impl_cleanup_v1", "d"))
        self.assertTrue(path.exists())
        self.git("update-ref", "refs/heads/" + p["branch"], p["commit"], p["source_commit"])
        foreign = path / "foreign"
        foreign.mkdir()
        (foreign / "evidence").write_text("retain\n")
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "impl_cleanup_v1", "d"))
        self.assertEqual((foreign / "evidence").read_text(), "retain\n")
        shutil.rmtree(foreign)
        build = path / "target"
        build.rename(path / "original-target")
        build.symlink_to(self.home)
        with self.assertRaises(A.Refused):
            self.call(self.action(p, "impl_cleanup_v1", "d"))
        self.assertTrue(self.home.exists())

    def repair(self, previous, key="e"):
        out = self.observe(previous, "implementation_outcome")
        self.assertEqual(out["outcome"], "done")
        observed = out["observation"]
        self.assertTrue(json.loads(observed["evidence"])["repair"])
        r = copy.deepcopy(self.prepare_req)
        r["intent_id"] = "sha256:" + key * 64
        candidate = r["inputs"]["candidate"]["value"]
        candidate["task_id"] = "repair-" + key
        candidate["result_id"] = "sha256:" + key * 64
        candidate["result"]["outputs"]["snapshot"]["snapshot_id"] = "sha256:" + ("1" if key == "e" else "2") * 64
        r["inputs"]["subject"] = self.text(candidate["result_id"])
        r["inputs"]["revision"] = self.text(candidate["result"]["outputs"]["snapshot"]["snapshot_id"])
        r["inputs"]["previous"] = self.action(previous)["inputs"]["prepared"]
        r["inputs"]["repair"] = dict(type="observation", value=observed)
        # Native accepted-source transport is the injected boundary here; real Git still
        # verifies the parent and raw candidate tree, then prepares an exact signed commit.
        def native(adapter, task, owner, parent):
            self.assertEqual(parent, previous)
            return dict(schema="af/scenario-impl-source@1", source_snapshot=A.digest(("rerooted-" + task["task_id"]).encode()), predecessor_snapshot=parent["snapshot_id"],
                        source_commit=parent["commit"], candidate_tree=parent["tree"],
                        transport_commit=parent["commit"], repository=str(self.repo),
                        candidate_snapshot=task["result"]["outputs"]["snapshot"]["snapshot_id"])
        # The fixed native transport branch names a complete candidate tree in this local fixture.
        self.git("update-ref", "refs/heads/af-impl-candidate", previous["commit"])
        with patch.object(LocalAdapter, "native_source", native):
            result = self.call(r)
        self.assertEqual(result["outcome"], "done")
        prepared = json.loads(result["evidence"])
        self.service.source = prepared
        return prepared

    def test_ci_and_review_failures_allow_two_exact_head_repairs_on_the_same_pr(self):
        initial = self.prepare()
        self.publish(initial)
        self.service.green = False
        first = self.repair(initial)
        self.assertEqual(first["source_commit"], initial["commit"])
        self.assertEqual(first["previous_owner"], initial["owner"])
        self.assertEqual(first["source_predecessor"], initial["snapshot_id"])
        self.assertNotEqual(first["source_snapshot"], initial["snapshot_id"])
        self.assertEqual(first["branch"], initial["branch"])
        self.assertEqual(self.call(self.action(first, key="f"))["outcome"], "done")
        self.assertEqual(self.service.posts, 1)
        with self.assertRaises(A.Refused):
            self.observe(initial, "checks_passed")
        self.service.green = True
        self.service.reviews = [dict(id=5, user=dict(id=8), state="CHANGES_REQUESTED", commit_id=first["commit"], body="Exercise the installed CLI")]
        second = self.repair(first, "9")
        self.assertEqual(self.call(self.action(second, key="8"))["outcome"], "done")
        self.assertEqual(self.service.posts, 1)
        self.service.green = False
        with self.assertRaises(A.Refused):
            self.repair(second, "6")
        self.assertEqual(self.service.posts, 1)
        self.service.green = True
        self.service.reviews = [dict(id=6, user=dict(id=8), state="APPROVED", commit_id=second["commit"])]
        self.assertEqual(self.observe(second, "implementation_ready")["outcome"], "done")
        self.merge()
        self.assertEqual(self.call(self.action(second, "impl_cleanup_v1", "7"))["outcome"], "done")
        for item in (initial, first, second):
            self.assertFalse(Path(item["worktree"]).exists())
        self.assertEqual(self.service.posts, 1)

    def test_unresolved_ci_reviews_and_conflicts_never_mean_completion(self):
        p = self.prepare()
        self.publish(p)
        self.service.green = False
        outcome = self.observe(p, "implementation_outcome")
        self.assertTrue(json.loads(outcome["evidence"])["repair"])
        self.assertEqual(self.observe(p, "implementation_ready")["outcome"], "failed")
        self.service.green = True
        self.service.reviews = [dict(id=2, user=dict(id=8), state="APPROVED", commit_id=p["commit"])]
        self.service.prs[0]["mergeable"] = False
        outcome = self.observe(p, "implementation_outcome")
        self.assertIn("conflicts", json.loads(outcome["evidence"])["reasons"][0])
        self.assertEqual(self.observe(p)["outcome"], "pending")
        self.assertTrue(Path(p["worktree"]).exists())

    def test_lost_cleanup_response_recovers_without_deleting_replacement_paths(self):
        p = self.prepare()
        self.publish(p)
        self.merge()
        request = self.action(p, "impl_cleanup_v1", "d")
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
            self.call(request, "recover")
        self.assertEqual((path / "foreign").read_text(), "preserve")
        # Removing this test-owned replacement permits the original cleanup intent to reconcile.
        shutil.rmtree(path)
        self.assertEqual(self.call(request, "reconcile")["outcome"], "pending")
        self.assertEqual(self.call(request, "recover")["outcome"], "done")

    def test_reusable_packages_wire_public_testing_evidence_into_the_evaluator(self):
        output = self.root / "packages"
        def hashed(args, **kwargs):
            self.assertEqual(args[1:3], ["catalog", "digest"])
            return subprocess.CompletedProcess(args, 0, stdout=D + "\n", stderr="")
        with patch.object(B.subprocess, "run", hashed):
            B.packages(output, Path(self.config["af"]), ["local"])
        correct = (output / "correctness/worker.toml").read_text()
        self.assertIn('[signature.contract.outputs.testing]', correct)
        self.assertTrue((output / "correctness/outputs/testing.schema.json").is_file())
        evaluator = (output / "evaluator/worker.toml").read_text()
        self.assertIn('af/ImplementationEvaluationInput@1', evaluator)
        self.assertIn('[signature.contract.inputs.testing]', evaluator)
        self.assertIn('"source", "testing"]', evaluator)
        root = (output / "implementation/pipeline.toml").read_text()
        review = (output / "review/pipeline.toml").read_text()
        self.assertIn('[nodes.inputs.testing]', review)
        self.assertIn('evaluator = "evaluator"', root)
        self.assertIn('port = "evaluation"', root)
        self.assertIn('[outputs.testing]', root)
        self.assertIn('[outputs.testing]', (output / "review/pipeline.toml").read_text())
        schema = json.loads((output / "evaluator/input.schema.json").read_text())
        self.assertIn('testing', schema['required'])
        self.assertEqual(schema['properties']['testing']['items']['properties']['artifact_type']['const'], 'af/ImplementationTestEvidence@1')

    def test_schema_writers_match_current_files_and_old_requests_fail_closed(self):
        for name, value in A.impl_schema_documents().items():
            self.assertEqual(json.loads((ROOT / "schemas" / name).read_text()), value)
        old = copy.deepcopy(self.prepare_req)
        old["schema"] = "af/scenario-adapter-request@2"
        with self.assertRaises(A.Refused):
            self.call(old)
        state_path = Path(self.config["state_dir"]) / "state.json"
        raw = A.decode(state_path.read_bytes(), A.STATE_LIMIT)
        raw["checksum"] = D
        state_path.write_bytes(A.canonical(raw))
        with self.assertRaises(A.Refused):
            self.call(self.prepare_req)

    def test_installed_wire_is_pinned_isolated_and_rejects_mutating_observe(self):
        self.assertIn(b"class ImplAdapter", self.executable.read_bytes())
        r = copy.deepcopy(self.prepare_req)
        r["method"] = "observe"
        out = subprocess.run([str(self.executable)], input=A.canonical(r), capture_output=True,
            env=dict(HOME=str(self.home), XDG_CONFIG_HOME=str(self.home), PYTHONPATH="/foreign", HTTPS_PROXY="https://foreign.invalid"))
        self.assertEqual(out.returncode, 1)
        self.assertNotIn(b"TEST_CREDENTIAL", out.stdout + out.stderr)
        self.assertEqual(self.service.posts, 0)


if __name__ == "__main__":
    unittest.main()
