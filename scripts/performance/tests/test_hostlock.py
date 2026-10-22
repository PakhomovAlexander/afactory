"""The machine coordination lock (ADR-0169).

Independent processes, each with its own private TMPDIR, HOME, XDG directories and working
directory, meet at the one machine lock: the second refuses while the first holds it, and a
shard refuses before any preflight touches the scratch filesystem or a container runtime.
Some tests take the real `/tmp/af-perf-machine.lock` for about a second; a holder that finds
it busy (another shard or test run on this machine) waits for it rather than failing.
"""
import json
import os
import subprocess
import sys

from perflab import hostlock, runner
from tests import test_runner
from tests.support import CLI, ROOT, TempDir

LAB = os.path.join(ROOT, "scripts", "performance")

HELPER = r'''
import json, os, sys, tempfile, time
sys.path.insert(0, %r)
from perflab import hostlock
mode, directory = sys.argv[1], (None if sys.argv[2] == "-" else sys.argv[2])
if mode == "path":
    print(json.dumps({"path": hostlock.default_path(), "tmpdir": tempfile.gettempdir(),
                      "home": os.environ["HOME"]}))
elif mode == "hold":
    deadline = time.monotonic() + 120
    while True:
        try:
            lock = hostlock.MachineLock(directory)
            break
        except hostlock.Refused as refused:
            if refused.reason != "concurrent_shard" or time.monotonic() > deadline:
                raise
            time.sleep(0.2)
    print("held " + lock.path, flush=True)
    sys.stdin.readline()
    lock.release()
    print("released", flush=True)
else:
    try:
        lock = hostlock.MachineLock(directory)
    except hostlock.Refused as refused:
        print(json.dumps({"refused": refused.reason, "detail": str(refused)}))
        sys.exit(0)
    print(json.dumps({"held": lock.path}))
    lock.release()
''' % LAB


class MachineLock(TempDir):
    def private_environment(self, name):
        """A process's own TMPDIR, HOME, XDG directories and working directory."""
        root = self.path(name)
        for sub in ("tmp", "home", "xdg", "cwd"):
            os.makedirs(os.path.join(root, sub))
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LC_ALL": "C",
               "TMPDIR": os.path.join(root, "tmp"), "HOME": os.path.join(root, "home")}
        for key in ("XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"):
            env[key] = os.path.join(root, "xdg")
        return env, os.path.join(root, "cwd")

    def helper(self, name, mode, directory="-", **kwargs):
        env, cwd = self.private_environment(name)
        return subprocess.Popen([sys.executable, "-I", "-B", "-c", HELPER, mode, directory],
                                env=env, cwd=cwd, stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)

    def answer(self, name, mode, directory="-"):
        child = self.helper(name, mode, directory)
        out, err = child.communicate(timeout=60)
        self.assertEqual(child.returncode, 0, err.decode())
        return json.loads(out)

    def hold(self, name, directory="-"):
        holder = self.helper(name, "hold", directory)
        self.addCleanup(self.release, holder)
        line = holder.stdout.readline().decode()
        if not line.startswith("held "):
            holder.kill()
            _, err = holder.communicate()
            self.fail("the holder did not take the lock: %r %s" % (line, err.decode()))
        return holder, line[len("held "):].strip()

    def release(self, holder):
        """Tell a holder to release; its remaining output, once it has exited."""
        if holder.returncode is None:
            out, _ = holder.communicate(b"\n", timeout=60)
            holder.released = out.decode()
        return getattr(holder, "released", "")

    def test_the_lock_path_is_the_same_whatever_a_process_s_private_roots(self):
        first = self.answer("one", "path")
        second = self.answer("two", "path")
        self.assertNotEqual(first["tmpdir"], second["tmpdir"])
        self.assertNotEqual(first["home"], second["home"])
        self.assertEqual(first["path"], second["path"])
        self.assertEqual(first["path"], os.path.join(os.path.realpath("/tmp"),
                                                     "af-perf-machine.lock"))
        self.assertEqual(first["path"], hostlock.default_path())

    def test_independent_processes_with_different_private_roots_never_both_hold_it(self):
        locks = self.path("locks")
        os.mkdir(locks, 0o755)
        holder, path = self.hold("holder", locks)
        self.assertEqual(path, os.path.join(os.path.realpath(locks), hostlock.MACHINE_LOCK_NAME))
        refused = self.answer("other", "try", locks)
        self.assertEqual(refused["refused"], "concurrent_shard", refused)
        self.assertEqual(self.release(holder).strip(), "released")
        # The lock file stays for the next holder; its inode is never removed.
        info = os.lstat(path)
        self.assertEqual(self.answer("third", "try", locks)["held"], path)
        self.assertEqual(os.lstat(path).st_ino, info.st_ino)

    def test_a_shard_refuses_before_any_preflight_while_another_process_holds_it(self):
        locks = self.path("locks")
        os.mkdir(locks, 0o755)
        holder, _ = self.hold("holder", locks)
        repo, base, head = test_runner.authority_repo(self.path("checkout"))
        plan = test_runner.plan_for(repo, base, head, ["source_capture"], run_id="contended",
                                    environment="linux_x86_64_musl_hosted")
        plan_path = self.path("plan.json")
        from perflab import contracts
        with open(plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        build = test_runner.make_build(self.path("build"), plan,
                                       {"baseline": test_runner.VERSION,
                                        "candidate": test_runner.VERSION})
        # A docker stand-in that records any call: the preflight would ask it first.
        docker = self.path("docker")
        with open(docker, "w") as stream:
            stream.write("#!/bin/sh\necho \"$@\" >> %s.calls\nexit 1\n" % docker)
        os.chmod(docker, 0o755)
        os.mkdir(self.path("scratch-fs"))
        options = runner.Options(plan=plan_path, build=build, shard=0,
                                 run_dir=self.path("run"), repo=repo.path,
                                 mode=runner.LINUX_CONTAINER, image="sha256:" + "1" * 64,
                                 scratch_fs=self.path("scratch-fs"), docker=docker,
                                 platform=("linux", "x86_64"), lock_directory=locks)
        with self.assertRaises(runner.Unavailable) as caught:
            runner.run_shard(options)
        self.assertEqual(caught.exception.reason, "concurrent_shard")
        self.assertFalse(os.path.exists(docker + ".calls"))
        self.assertFalse(os.path.exists(self.path("run")))
        self.assertEqual(os.listdir(self.path("scratch-fs")), [])
        self.release(holder)
        # Released, the same shard proceeds to its preflight and refuses there instead.
        with self.assertRaises(runner.Unavailable) as caught:
            runner.run_shard(options)
        self.assertEqual(caught.exception.reason, "no_container_runtime")
        self.assertTrue(os.path.exists(docker + ".calls"))

    def test_a_scratch_enforcement_fill_takes_the_machine_lock(self):
        locks = self.path("locks")
        os.mkdir(locks, 0o755)
        holder, _ = self.hold("holder", locks)
        env, cwd = self.private_environment("capabilities")
        bootstrap = ("import runpy,sys; sys.path.insert(0,%r); "
                     "from perflab import runner; original=runner.machine_lock; "
                     "runner.machine_lock=lambda: original(%r); "
                     "sys.argv=[%r]+sys.argv[1:]; runpy.run_path(%r,run_name='__main__')"
                     % (LAB, locks, CLI, CLI))
        result = subprocess.run([sys.executable, "-I", "-B", "-c", bootstrap, "capabilities",
                                 "--suite", os.path.join(ROOT, "fixtures/performance/suite.toml"),
                                 "--scratch", self.path("scratch-fs"),
                                 "--verify-scratch-enforcement"],
                                env=env, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 4, result.stderr.decode())
        self.assertEqual(json.loads(result.stdout)["reason"], "concurrent_shard")
        self.release(holder)

    def test_an_unsafe_lock_path_or_directory_refuses_and_is_left_alone(self):
        target = self.path("target")
        with open(target, "w") as stream:
            stream.write("not a lock\n")
        cases = []
        linked = self.path("symlink")
        os.mkdir(linked)
        os.symlink(target, os.path.join(linked, hostlock.MACHINE_LOCK_NAME))
        cases.append(linked)
        directory = self.path("directory")
        os.makedirs(os.path.join(directory, hostlock.MACHINE_LOCK_NAME))
        cases.append(directory)
        hardlinked = self.path("hardlinked")
        os.mkdir(hardlinked)
        os.link(target, os.path.join(hardlinked, hostlock.MACHINE_LOCK_NAME))
        cases.append(hardlinked)
        open_dir = self.path("world-writable")
        os.mkdir(open_dir)
        os.chmod(open_dir, 0o777)
        cases.append(open_dir)
        for case in cases:
            with self.subTest(case=os.path.basename(case)):
                with self.assertRaises(hostlock.Refused) as caught:
                    hostlock.MachineLock(case)
                self.assertEqual(caught.exception.reason, "coordination_unavailable")
        self.assertTrue(os.path.islink(os.path.join(linked, hostlock.MACHINE_LOCK_NAME)))
        with open(target) as stream:
            self.assertEqual(stream.read(), "not a lock\n")
        # A sticky world-writable directory is the system's shape and is accepted.
        sticky = self.path("sticky")
        os.mkdir(sticky)
        os.chmod(sticky, 0o1777)
        with hostlock.MachineLock(sticky) as lock:
            self.assertEqual(os.stat(lock.path).st_mode & 0o777, 0o644)
        # Production refuses a lock directory the system does not own.
        with self.assertRaises(hostlock.Refused):
            hostlock._check_directory(self.tmp, True)
