"""The retained p8 local evidence's own tools (ADR-0188): the workflow's snapshot stage and
failure semantics, on a tiny real Git repository, and the post-export check of what an export
actually holds. Nothing here builds or measures a workload."""
import importlib.util
import json
import os
import shutil
import subprocess
import unittest

from tests import support

EVIDENCE = os.path.join(support.ROOT, "docs/performance-evidence/p8-local")
TOOLS = os.path.join(EVIDENCE, "tools")
WORKFLOW = os.path.join(TOOLS, "workflow.sh")
# The workflow runs with PATH reduced to the system directories.
SYSTEM_TOOLS = all(any(os.access(os.path.join(directory, tool), os.X_OK)
                       for directory in ("/usr/bin", "/bin")) for tool in ("git", "python3"))


def load_checks():
    spec = importlib.util.spec_from_file_location("p8_local_checks",
                                                  os.path.join(TOOLS, "checks.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_fingerprint():
    spec = importlib.util.spec_from_file_location("p8_local_fingerprint",
                                                  os.path.join(TOOLS, "fingerprint.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    # A constructed tree has no laboratory to compute its harness digest.
    module.harness_digest = lambda root: "sha256:" + "0" * 64
    return module


def git(cwd, *args):
    env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
               GIT_AUTHOR_NAME="Source", GIT_AUTHOR_EMAIL="source@example.invalid",
               GIT_COMMITTER_NAME="Source", GIT_COMMITTER_EMAIL="source@example.invalid")
    return subprocess.run(["git"] + list(args), cwd=cwd, env=env, check=True,
                          stdout=subprocess.PIPE).stdout.decode().strip()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as stream:
        stream.write(text)


@unittest.skipUnless(SYSTEM_TOOLS, "git and python3 are not in /usr/bin or /bin")
class Workflow(support.TempDir):
    def source(self):
        """A clean checkout: two tracked files, an ignored build target and its `.git`."""
        source = self.path("source")
        write(os.path.join(source, "a.txt"), "a\n")
        write(os.path.join(source, "sub", "b.txt"), "b\n")
        write(os.path.join(source, ".gitignore"), "/target/\n")
        git(source, "init", "-q", "-b", "main")
        git(source, "add", "-A")
        git(source, "commit", "-q", "-m", "source")
        write(os.path.join(source, "target", "built"), "x\n")
        return source

    def stage(self, source, work, stage):
        env = dict(os.environ, REAL_HOME=self.path("real-home"))
        return subprocess.run(["bash", WORKFLOW, source, work, stage], env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def assert_fresh(self, source, work):
        repo = os.path.join(work, "repo")
        self.assertTrue(os.path.isdir(os.path.join(repo, ".git")))
        self.assertEqual(git(repo, "rev-list", "--count", "HEAD"), "1")
        self.assertEqual(git(repo, "log", "-1", "--format=%an"), "af p8 workflow")
        head = git(repo, "rev-parse", "HEAD")
        self.assertNotEqual(head, git(source, "rev-parse", "HEAD"))
        with open(os.path.join(work, "commit")) as stream:
            self.assertEqual(stream.read().strip(), head)
        self.assertEqual(git(repo, "ls-files").split("\n"), [".gitignore", "a.txt", "sub/b.txt"])
        self.assertFalse(os.path.exists(os.path.join(repo, "target")))

    def test_a_clean_checkout_is_committed_in_a_fresh_repository_of_its_own(self):
        source = self.source()
        before = git(source, "rev-parse", "HEAD")
        done = self.stage(source, self.path("work"), "snapshot")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assert_fresh(source, self.path("work"))
        # The source is only read.
        self.assertEqual(git(source, "rev-parse", "HEAD"), before)
        self.assertEqual(git(source, "status", "--porcelain"), "")

    def test_a_worktree_s_git_file_is_never_inherited(self):
        source = self.source()
        worktree = self.path("worktree")
        git(source, "worktree", "add", "-q", worktree)
        self.assertTrue(os.path.isfile(os.path.join(worktree, ".git")))
        done = self.stage(worktree, self.path("work"), "snapshot")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assert_fresh(worktree, self.path("work"))

    def test_an_existing_destination_or_a_failed_git_step_stops_the_snapshot(self):
        source = self.source()
        work = self.path("work")
        self.assertEqual(self.stage(source, work, "snapshot").returncode, 0)
        with open(os.path.join(work, "commit")) as stream:
            first = stream.read()
        again = self.stage(source, work, "snapshot")
        self.assertNotEqual(again.returncode, 0)
        self.assertIn(b"already exists", again.stderr)
        with open(os.path.join(work, "commit")) as stream:
            self.assertEqual(stream.read(), first)
        # Nothing to commit: the commit fails, and no commit identity is written.
        empty = self.path("empty")
        os.makedirs(empty)
        failed = self.stage(empty, self.path("work-empty"), "snapshot")
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn(b"git commit failed", failed.stderr)
        self.assertFalse(os.path.exists(self.path("work-empty", "commit")))

    def test_a_failed_step_ends_its_stage_with_its_status(self):
        source = self.source()
        work = self.path("work")
        self.assertEqual(self.stage(source, work, "snapshot").returncode, 0)
        # The tiny source has no laboratory, so the plan stage's first step fails.
        done = self.stage(source, work, "plan")
        self.assertEqual(done.returncode, 2, done.stderr)
        with open(os.path.join(work, "steps.jsonl")) as stream:
            steps = [json.loads(line) for line in stream]
        self.assertEqual([(step["step"], step["exit"]) for step in steps], [("capabilities", 2)])
        self.assertFalse(os.path.exists(os.path.join(work, "logs", "list.out")))
        self.assertEqual(self.stage(source, work, "no-such-stage").returncode, 2)


class Exported(support.TempDir):
    def test_the_retained_directory_resolves_every_link_and_holds_every_listed_file(self):
        checks = load_checks()
        out = self.path("links-exported.json")
        self.assertEqual(checks.exported(EVIDENCE, out), 0)
        result = json.loads(support.read(out))
        self.assertEqual(result["failures"], [])
        self.assertIn("runs/p8_local_aa/report.html", result["pages"])
        self.assertEqual(len(result["previously_omitted"]), 10)
        self.assertTrue(all(item["matches"] for item in result["previously_omitted"]))
        self.assertEqual(result["manifest_failures"], [])
        # The committed post-export record is this check's result.
        self.assertEqual(result, json.loads(support.read(
            os.path.join(EVIDENCE, "checks", "links-exported.json"))))

    def work(self):
        """A minimal workflow directory: a report with one download, a history of two runs and
        every file the exporter copies."""
        work = self.path("work")
        write(os.path.join(work, "report", "evidence", "run.json"), "{}\n")
        write(os.path.join(work, "report", "report.html"),
              '<a href="evidence/run.json">run</a>')
        write(os.path.join(work, "report", "report.json"), json.dumps({
            "downloads": [{"path": "evidence/run.json", "sha256": "sha256:ca3d163bab055381827226"
                           "140568f3bef7eaac187cebd76878e0b63e9e442356"}],
            "diagnostics": []}))
        for run in ("p8_local_aa", "p8_local_aa_rerun"):
            write(os.path.join(work, "history-2", "runs", run, "report.html"), "<p>run</p>")
        write(os.path.join(work, "history-2", "history.html"),
              '<a href="runs/p8_local_aa/report.html">a</a>'
              '<a href="runs/p8_local_aa_rerun/report.html">b</a>')
        for name in ("steps.jsonl", "fetch-from-first-attempt.err", "reproduce.json",
                     "links.json", "browser.json", "sources-digest-timing.json",
                     "load-before-run.txt", "load-after-run.txt", "load-before-rerun.txt",
                     "load-after-rerun.txt", "reproduce.argv", "native-kernel-test.txt"):
            write(os.path.join(work, name), "%s at %s\n" % (name, os.path.realpath(work)))
        write(self.path("receipts", "receipts-1.json"), "{}\n")
        os.makedirs(os.path.join(work, "logs"))
        return work

    def export(self, work):
        return subprocess.run(
            ["python3", "-I", "-B", os.path.join(TOOLS, "export.py"), work, self.path("dest"),
             self.path("home"), TOOLS], stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def test_an_export_keeps_every_linked_run_page_and_checks_what_it_wrote(self):
        work = self.work()
        done = self.export(work)
        self.assertEqual(done.returncode, 0, done.stderr)
        dest = self.path("dest")
        self.assertTrue(os.path.isfile(os.path.join(dest, "history", "runs", "p8_local_aa",
                                                    "report.html")))
        self.assertFalse(os.path.exists(os.path.join(dest, "omitted.json")))
        result = json.loads(support.read(os.path.join(dest, "checks", "links-exported.json")))
        self.assertEqual((result["failures"], result["links_checked"]), ([], 3))
        self.assertEqual(support.read(os.path.join(dest, "steps.jsonl")),
                         b"steps.jsonl at $WORK\n")
        # A history linking a page the export does not hold fails the export.
        shutil.rmtree(os.path.join(work, "history-2", "runs", "p8_local_aa"))
        failed = self.export(work)
        self.assertEqual(failed.returncode, 1, failed.stderr)
        result = json.loads(support.read(os.path.join(dest, "checks", "links-exported.json")))
        self.assertEqual([item["target"] for item in result["failures"]],
                         ["runs/p8_local_aa/report.html"])


class Reconciliation(support.TempDir):
    """A further repair is reconciled with the finalized tree through the earlier repair's
    record (ADR-0189): neither earlier record changes, and `verify-repair` checks the new one."""

    def tree(self, name, contents):
        root = self.path(name)
        for path, text in contents.items():
            write(os.path.join(root, path), text)
        return root

    def test_a_second_repair_is_reconciled_with_the_finalized_tree(self):
        fingerprint = load_fingerprint()
        measured = {"a.txt": "a\n", "b.txt": "b\n", "c.txt": "c\n"}
        finalized = dict(measured, **{"a.txt": "a finalized\n"})
        first = dict(finalized, **{"b.txt": "b repaired\n", "d.txt": "d added\n"})
        second = dict(first, **{"c.txt": "c repaired\n", "d.txt": "d repaired\n",
                                "e.txt": "e added\n"})
        lines = dict((path, "%d %s" % item) for path, item
                     in fingerprint.files(self.tree("measured", measured)).items())
        write(self.path("full.json"), json.dumps({"lines": lines}))
        finalized_root = self.tree("finalized", finalized)
        retained = self.path("retained.json")
        write(retained, json.dumps({"reconciliation": fingerprint.reconcile(
            finalized_root, self.path("full.json"))}))
        first_root = self.tree("first", first)
        prior = self.path("prior.json")
        write(prior, json.dumps(fingerprint.repair(first_root, finalized_root, retained)))
        self.assertTrue(fingerprint.verify_repair(first_root, retained, prior)["matches"])
        # The finalized tree is gone; the second repair is reconciled through the first record.
        shutil.rmtree(finalized_root)
        second_root = self.tree("second", second)
        self.assertFalse(fingerprint.verify_repair(second_root, retained, prior)["matches"])
        record = fingerprint.rerepair(second_root, first_root, retained, prior)
        self.assertEqual(
            [(item["path"], item["finalized"] is None) for item in record["changed_paths"]],
            [("b.txt", False), ("c.txt", False), ("d.txt", True), ("e.txt", True)])
        self.assertEqual(record["changed_paths"][1]["finalized"], lines["c.txt"])
        self.assertEqual(record["unchanged_files"], 1)
        write(self.path("record.json"), json.dumps(record))
        self.assertTrue(fingerprint.verify_repair(second_root, retained,
                                                  self.path("record.json"))["matches"])
        self.assertFalse(fingerprint.verify_repair(first_root, retained,
                                                   self.path("record.json"))["matches"])
        # A byte that no record explains is refused.
        write(os.path.join(second_root, "a.txt"), "a changed again\n")
        self.assertFalse(fingerprint.verify_repair(second_root, retained,
                                                   self.path("record.json"))["matches"])
        # The prior tree must be the one the prior record reconciled.
        with self.assertRaises(SystemExit):
            fingerprint.rerepair(second_root, second_root, retained, prior)
