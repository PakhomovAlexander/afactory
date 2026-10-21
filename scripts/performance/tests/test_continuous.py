"""Build once, run bounded shards from the moved build, aggregate what arrived (ADR-0177).

The flow the workflow runs, locally with its actual commands: a verified build is packed, moved
through a ZIP the way Actions moves an artifact (every mode lost), unpacked and verified, and
its shards run from it without rebuilding, with the runner's stand-in binaries in trusted local
mode. Each shard's evidence is staged as its transport artifact and the aggregation imports
whatever arrived. Nothing here is a hosted run or a measurement of AF; the stand-ins' numbers
mean nothing, and the ZIP round trip stands in for Actions' own transport.
"""
import io
import os
import stat
import subprocess
import sys
import tarfile
import zipfile

from perflab import buildpack, ci, contracts, credentials, hostprep, runner, transport
from perflab.values import ContractError, sha256_bytes
from tests import test_runner
from tests.support import CLI, ROOT, TempDir, read

DISCOVERY = test_runner.DISCOVERY
SMALL = test_runner.SMALL


def through_actions(source, target):
    """What an artifact upload then download does to a directory: its files zipped at the root
    and written back as 0644 files."""
    data = io.BytesIO()
    with zipfile.ZipFile(data, "w", zipfile.ZIP_DEFLATED) as container:
        for directory, _, files in os.walk(source):
            for name in sorted(files):
                path = os.path.join(directory, name)
                container.write(path, os.path.relpath(path, source))
    os.makedirs(target)
    with zipfile.ZipFile(io.BytesIO(data.getvalue())) as container:
        for info in container.infolist():
            path = os.path.join(target, *info.filename.split("/"))
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as stream:
                stream.write(container.read(info))
            os.chmod(path, 0o644)
    return data.getvalue()


class BuildOnce(test_runner.ShardHarness):

    def moved_build(self, plan, plan_path, build, name="build"):
        packed = self.path(name + "-pack")
        result = subprocess.run([sys.executable, "-I", CLI, "ci-pack", "--plan", plan_path,
                                 "--build", build, "--output", packed], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        downloaded = self.path(name + "-downloaded")
        through_actions(packed, downloaded)
        unpacked = self.path(name + "-unpacked")
        result = subprocess.run([sys.executable, "-I", CLI, "ci-unpack", "--plan", plan_path,
                                 "--pack", downloaded, "--output", unpacked], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        return unpacked, downloaded

    def shard(self, plan_path, build, index):
        run_dir = self.path("host-%d" % index)
        doc = runner.run_shard(self.options(plan_path, build, shard=index, run_dir=run_dir))
        staged = self.path("staged-%d" % index)
        transport.stage(staged, run_dir, "shard_bundle", shard=index)
        return doc, staged

    def test_one_build_moves_to_every_shard_and_aggregates_without_rebuilding(self):
        plan, plan_path, build = self.prepare([DISCOVERY, SMALL], run_id="moved", shards=2)
        unpacked, downloaded = self.moved_build(plan, plan_path, build)
        # Actions dropped every mode; the unpack restored them only after verifying bytes.
        self.assertFalse(os.stat(os.path.join(downloaded, "build.tar")).st_mode & 0o111)
        supervisor = os.path.join(unpacked, "artifacts", "supervisor", "perf-supervisor")
        self.assertEqual(stat.S_IMODE(os.stat(supervisor).st_mode), buildpack.EXECUTABLE_MODE)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(unpacked, "builds.json")).st_mode),
                         buildpack.FILE_MODE)
        self.assertEqual(read(os.path.join(unpacked, "builds.json")),
                         read(os.path.join(build, "builds.json")))
        shards = self.path("shards")
        os.mkdir(shards)
        for index in (0, 1):
            doc, staged = self.shard(plan_path, unpacked, index)
            self.assertEqual(doc["status"], "complete", doc["failure"])
            through_actions(staged, os.path.join(shards, ci.shard_artifact("moved", index)))
        report_doc, completeness = ci.aggregate(
            read(plan_path), buildpack.read_member(downloaded, plan, "builds.json"), shards,
            self.path("aggregate-run"), self.path("report"), self.path("summary.md"),
            self.path("run-transport"))
        self.assertEqual(completeness, {"planned": 2, "received": [0, 1], "missing": []})
        self.assert_valid_evidence(report_doc["verdict"])
        self.assertIn("### AF performance", read(self.path("summary.md")).decode())
        # The run bundle travels in the collector's own format, read back from its ZIP.
        zipped = through_actions(self.path("run-transport"), self.path("run-downloaded"))
        doc, files, views = transport.read_transport(zipped)
        self.assertEqual((doc["run_id"], doc["content"]), ("moved", "run_bundle"))
        self.assertEqual(sorted(name for name, _ in views), ["report.html", "summary.md"])
        self.assertIn("builds.json", files)

    def test_missing_and_failed_shards_stay_incomplete_evidence(self):
        plan, plan_path, build = self.prepare([DISCOVERY, SMALL], run_id="partial", shards=2,
                                              overrides={"3": "uncertain"})
        unpacked, downloaded = self.moved_build(plan, plan_path, build)
        shards = self.path("shards")
        os.mkdir(shards)
        failed, staged = self.shard(plan_path, unpacked, 0)
        self.assertEqual((failed["status"], failed["failure"]["reason"]),
                         ("failed", "cleanup_uncertain"))
        # The failed shard's partial evidence and terminal manifest still travel.
        through_actions(staged, os.path.join(shards, ci.shard_artifact("partial", 0)))
        report_doc, completeness = ci.aggregate(
            read(plan_path), buildpack.read_member(downloaded, plan, "builds.json"), shards,
            self.path("aggregate-run"), self.path("report"), self.path("summary.md"))
        self.assertEqual(completeness["missing"], [1])
        verdict = report_doc["verdict"]
        self.assertEqual(verdict["aggregate"]["result"], "incomplete")
        self.assertEqual([entry["status"] for entry in verdict["shards"]], ["failed", None])
        summary = read(self.path("summary.md")).decode()
        self.assertIn("Shards without an artifact, kept as incomplete evidence: 1", summary)

    def test_a_single_download_is_flat_and_still_bound_to_its_plan(self):
        plan, plan_path, build = self.prepare([SMALL], run_id="flat", shards=1)
        _, staged = self.shard(plan_path, build, 0)
        flat = self.path("flat-download")
        through_actions(staged, flat)
        doc, completeness = ci.aggregate(read(plan_path), read(os.path.join(build, "builds.json")),
                                         flat, self.path("flat-run"), self.path("flat-report"))
        self.assertEqual(completeness, {"planned": 1, "received": [0], "missing": []})
        self.assert_valid_evidence(doc["verdict"])
        # A second named artifact alongside the flat one is not a second valid layout.
        through_actions(staged, os.path.join(flat, ci.shard_artifact("flat", 0)))
        with self.assertRaises(ContractError):
            ci.aggregate(read(plan_path), read(os.path.join(build, "builds.json")), flat,
                         self.path("mixed-run"), self.path("mixed-report"))

    def test_the_only_downloaded_shard_need_not_be_zero(self):
        plan, plan_path, build = self.prepare([DISCOVERY, SMALL], run_id="survivor", shards=2)
        _, staged = self.shard(plan_path, build, 1)
        flat = self.path("only-download")
        through_actions(staged, flat)
        doc, completeness = ci.aggregate(read(plan_path), read(os.path.join(build, "builds.json")),
                                         flat, self.path("only-run"), self.path("only-report"))
        self.assertEqual(completeness, {"planned": 2, "received": [1], "missing": [0]})
        self.assertEqual(doc["verdict"]["aggregate"]["result"], "incomplete")

    def test_foreign_duplicate_and_unplanned_shard_artifacts_are_refused(self):
        plan, plan_path, build = self.prepare([DISCOVERY, SMALL], run_id="guarded", shards=2)
        _, staged = self.shard(plan_path, build, 0)
        builds_bytes = read(os.path.join(build, "builds.json"))
        for names, needle in (
                (["af-perf-other_run-shard-0"], "is not a shard artifact of run guarded"),
                (["af-perf-guarded-shard-5"], "the plan has 2"),
                (["af-perf-guarded-shard-1"], "not shard 1 of this run"),
                (["af-perf-guarded-shard-0", "af-perf-guarded-shard-00"], "arrived twice")):
            with self.subTest(names=names):
                shards = self.path("shards-%s" % names[-1])
                os.mkdir(shards)
                for name in names:
                    through_actions(staged, os.path.join(shards, name))
                with self.assertRaises(ContractError) as caught:
                    ci.aggregate(read(plan_path), builds_bytes, shards,
                                 self.path("run-" + names[-1]), self.path("report-" + names[-1]))
                self.assertIn(needle, str(caught.exception))


class HostilePacks(TempDir):
    """A pack must be exactly the verified build its manifest names."""

    def setUp(self):
        TempDir.setUp(self)
        self.plan = {"run_id": "packed", "plan_digest": "sha256:" + "a" * 64,
                     "limits": {"build_artifact_bytes": 1 << 20}}
        os.makedirs(self.path("build", "artifacts"))
        with open(self.path("build", "builds.json"), "w") as stream:
            stream.write("{}")
        with open(self.path("build", "artifacts", "tool"), "w") as stream:
            stream.write("#!/bin/sh\necho tool\n")
        os.chmod(self.path("build", "artifacts", "tool"), 0o550)
        self.doc = buildpack.pack(self.path("build"), self.plan, self.path("pack"))

    def replace_archive(self, members, update_manifest=True):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w") as tar:
            for name, body, kind in members:
                info = tarfile.TarInfo(name)
                info.type = kind
                if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                    info.linkname = body
                    tar.addfile(info)
                else:
                    info.size = len(body)
                    tar.addfile(info, io.BytesIO(body))
        raw = data.getvalue()
        os.chmod(self.path("pack", buildpack.ARCHIVE), 0o644)
        with open(self.path("pack", buildpack.ARCHIVE), "wb") as stream:
            stream.write(raw)
        if update_manifest:
            doc = dict(self.doc, archive={"name": "build.tar", "size": len(raw),
                                          "sha256": sha256_bytes(raw)})
            os.chmod(self.path("pack", buildpack.MANIFEST), 0o644)
            with open(self.path("pack", buildpack.MANIFEST), "wb") as stream:
                stream.write(contracts.write_bytes(doc, "build-pack"))

    def refused(self, needle):
        with self.assertRaises(ContractError) as caught:
            buildpack.unpack(self.path("pack"), self.plan, self.path("out"))
        self.assertIn(needle, str(caught.exception))
        self.assertFalse(os.path.lexists(self.path("out")), "nothing is left of a refusal")

    def test_the_round_trip_restores_modes_after_verifying_bytes(self):
        buildpack.unpack(self.path("pack"), self.plan, self.path("out"))
        tool = self.path("out", "artifacts", "tool")
        self.assertEqual(stat.S_IMODE(os.stat(tool).st_mode), 0o555)
        self.assertEqual(subprocess.run([tool], stdout=subprocess.PIPE).stdout, b"tool\n")

    def test_another_plan_or_a_changed_archive_is_refused(self):
        with self.assertRaises(ContractError):
            buildpack.unpack(self.path("pack"), dict(self.plan, run_id="other"), self.path("o"))
        tool = read(self.path("build", "artifacts", "tool"))
        self.replace_archive([("builds.json", b"{}", tarfile.REGTYPE),
                              ("artifacts/tool", tool, tarfile.REGTYPE)], update_manifest=False)
        self.refused("is not the")

    def test_links_traversal_duplicates_and_unlisted_members_are_refused(self):
        tool = read(self.path("build", "artifacts", "tool"))
        cases = (
            ([("builds.json", b"{}", tarfile.REGTYPE),
              ("artifacts/tool", "/etc/passwd", tarfile.SYMTYPE)], "not a regular file"),
            ([("builds.json", b"{}", tarfile.REGTYPE),
              ("artifacts/tool", "builds.json", tarfile.LNKTYPE)], "not a regular file"),
            ([("builds.json", b"{}", tarfile.REGTYPE), ("../escape", b"x", tarfile.REGTYPE),
              ("artifacts/tool", tool, tarfile.REGTYPE)], "a build archive member"),
            ([("builds.json", b"{}", tarfile.REGTYPE), ("builds.json", b"{}", tarfile.REGTYPE),
              ("artifacts/tool", tool, tarfile.REGTYPE)], "twice"),
            ([("builds.json", b"{}", tarfile.REGTYPE), ("artifacts/tool", tool,
                                                        tarfile.REGTYPE),
              ("artifacts/extra", b"x", tarfile.REGTYPE)], "not in the pack's manifest"),
            ([("builds.json", b"{}", tarfile.REGTYPE),
              ("artifacts/tool", tool[:-1] + b"X", tarfile.REGTYPE)], "does not match"),
            ([("builds.json", b"{}", tarfile.REGTYPE)], "lacks artifacts/tool"),
        )
        for members, needle in cases:
            with self.subTest(needle=needle):
                self.replace_archive(members)
                self.refused(needle)

    def test_a_pack_never_follows_a_symlink_in_the_build(self):
        os.symlink("/etc/hosts", self.path("build", "artifacts", "link"))
        with self.assertRaises(ContractError) as caught:
            buildpack.pack(self.path("build"), self.plan, self.path("pack-2"))
        self.assertIn("symlink", str(caught.exception))


TOKEN = "ghs_" + "A1b2C3d4" * 5


class Credentials(TempDir):
    def test_a_persisted_checkout_credential_is_found_wherever_it_hides(self):
        os.makedirs(self.path("repo"))
        clean = '[core]\n\tbare = false\n[remote "origin"]\n\turl = https://github.com/o/r\n'
        with open(self.path("repo", "config"), "w") as stream:
            stream.write(clean)
        self.assertEqual(credentials.git_config(self.path("repo")), [])
        for text, needle in (
                ('[http "https://github.com/"]\n\textraheader = AUTHORIZATION: basic %s\n'
                 % TOKEN, "extraheader"),
                ('[includeIf "gitdir:/w/.git"]\n\tpath = /runner/temp/git-credentials.config\n',
                 "[includeif] section"),
                ("[remote \"origin\"]\n\turl = https://x-access-token:%s@github.com/o/r\n"
                 % TOKEN, "user information")):
            with self.subTest(needle=needle):
                with open(self.path("repo", "config"), "w") as stream:
                    stream.write(clean + text)
                findings = credentials.git_config(self.path("repo"))
                self.assertTrue(any(needle in item for item in findings), findings)
                self.assertFalse(any(TOKEN in item for item in findings), "never echoed")

    def test_environments_and_bundles_are_scanned_by_name_and_shape(self):
        self.assertEqual(credentials.environment({"PATH": "/usr/bin"}), [])
        found = credentials.environment({"GITHUB_TOKEN": TOKEN, "OTHER": "x" + TOKEN})
        self.assertEqual(found, ["the environment variable OTHER holds a token-shaped value",
                                 "the environment sets GITHUB_TOKEN"][::-1])
        os.makedirs(self.path("bundle", "logs"))
        with open(self.path("bundle", "logs", "a.log"), "wb") as stream:
            stream.write(b"x" * (1 << 20) + TOKEN.encode())
        self.assertEqual(credentials.tree(self.path("bundle"), "bundle"),
                         ["bundle/logs/a.log: a token-shaped value"])
        result = subprocess.run([sys.executable, "-I", CLI, "ci-credentials", "--scan",
                                 self.path("bundle")], stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 4)
        self.assertNotIn(TOKEN.encode(), result.stderr + result.stdout)


class StandInHost(object):
    """Records every step and performs only its effect on paths the test owns."""

    def __init__(self, free, fail=None):
        self.argv = []
        self.free = free
        self.fail = fail

    def run(self, argv, timeout=None):
        self.argv.append(argv)
        if self.fail is not None and self.fail in argv:
            return 1, "refused by the stand-in"
        if argv[0] == "truncate":
            open(argv[-1], "w").close()
        elif argv[0] == "mkdir":
            os.mkdir(argv[-1])
        elif argv[0] == "rmdir":
            os.rmdir(argv[-1])
        elif argv[:2] == ["rm", "-f"]:
            os.remove(argv[-1])
        return 0, ""

    def statvfs(self, path):
        class Info(object):
            f_bavail = self.free
            f_frsize = 1
        return Info()


class HostedScratch(TempDir):
    GIB = 1 << 30

    def host(self, stand_in, system="linux"):
        return hostprep.Host(run=stand_in.run, statvfs=stand_in.statvfs, system=system,
                             uid=1001, gid=118)

    def test_a_loop_mounted_scratch_is_prepared_and_removed(self):
        stand_in = StandInHost(13 * self.GIB)
        image, mount = self.path("scratch.img"), self.path("scratch-fs")
        record = hostprep.prepare(image, mount, 2 * self.GIB, self.host(stand_in))
        self.assertEqual(record["size_bytes"], 2 * self.GIB)
        self.assertIn(["sudo", "-n", "mount", "-o", "loop,nodev,nosuid", image, mount],
                      stand_in.argv)
        self.assertIn(["sudo", "-n", "chown", "1001:118", mount], stand_in.argv)
        self.assertEqual(hostprep.cleanup(image, mount, self.host(stand_in)), [])
        self.assertEqual(stand_in.argv[-3:], [["sudo", "-n", "umount", mount],
                                             ["rmdir", mount], ["rm", "-f", image]])
        self.assertFalse(os.path.lexists(image) or os.path.lexists(mount))

    def test_refusals_create_nothing_and_never_lower_the_floor(self):
        image, mount = self.path("scratch.img"), self.path("scratch-fs")
        for stand_in, system, reason in (
                (StandInHost(100 * self.GIB), "darwin", "not_linux"),
                (StandInHost(11 * self.GIB), "linux", "insufficient_disk"),
                (StandInHost(100 * self.GIB, fail="true"), "linux", "no_passwordless_sudo")):
            with self.subTest(reason=reason):
                with self.assertRaises(hostprep.Refused) as caught:
                    hostprep.prepare(image, mount, 2 * self.GIB, self.host(stand_in, system))
                self.assertEqual(caught.exception.reason, reason)
                self.assertFalse(os.path.lexists(image) or os.path.lexists(mount))
        with self.assertRaises(ContractError):
            hostprep.prepare(image, mount, 2 * self.GIB, self.host(StandInHost(1 << 40)),
                             floor_bytes=self.GIB)

    def test_a_failed_mount_removes_what_was_created(self):
        stand_in = StandInHost(100 * self.GIB, fail="mount")
        image, mount = self.path("scratch.img"), self.path("scratch-fs")
        with self.assertRaises(hostprep.Refused) as caught:
            hostprep.prepare(image, mount, 2 * self.GIB, self.host(stand_in))
        self.assertEqual(caught.exception.reason, "preparation_failed")
        self.assertFalse(os.path.lexists(image) or os.path.lexists(mount))
        self.assertNotIn(["sudo", "-n", "umount", mount], stand_in.argv)
