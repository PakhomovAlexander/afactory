"""Build orchestration against frozen plans (package p3a, ADR-0166).

Real Git fixtures and real detached worktrees; the toolchain is either the stand-in of
`tests.build_fixtures` (deterministic, runs everywhere) or, in `RealToolchain`, the repository's
pinned Rust building a minimal real workspace whose build.rs embeds its commit through the
worktree's `.git` file.
"""
import json
import os
import shutil
import platform
import subprocess
import time
import unittest
from unittest import mock

from perflab import adapters, builds, contracts, diagcontracts, diagnostics, tomlfile
from perflab import suite as suite_module
from perflab.values import ContractError, sha256_bytes
from tests import build_fixtures
from tests.build_fixtures import fixture_repo, fake_toolchain, plan_for
from tests.support import ROOT, SUITE, Repo, TempDir, read

PLENTY = lambda path: 1 << 50  # noqa: E731 - the free-disk probe of a test with room


def registry():
    return contracts.validate_adapters(tomlfile.loads(build_fixtures.ADAPTERS.encode("utf-8"),
                                                      "fixture adapters"))


class FakeBuild(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.repo, self.base, self.head, self.authority = fixture_repo(self.path("checkout"))
        self.toolchain = builds.Toolchain(fake_toolchain(self.path("toolchain")), "1.88.0")

    def build(self, baseline=None, candidate=None, run_id="fixture_build_run", **options):
        plan = plan_for(self.repo, baseline or self.base, candidate or self.head,
                        self.authority, run_id=run_id)
        options.setdefault("free_bytes", PLENTY)
        builder = builds.Builder(self.repo.path, plan, self.toolchain, **options)
        return builder, builder.build()

    def test_the_build_establishes_the_run_s_one_execution_start_and_total_deadline(self):
        # A deterministic wall clock: the build is the run's first execution (ADR-0170).
        builder, doc = self.build(wall=lambda: 1800000000.0)
        execution = doc["execution"]
        self.assertEqual(execution["run"], {"started_unix_ms": 1800000000000,
                                            "deadline_unix_ms": 1800000000000 + 120 * 60000,
                                            "total_minutes": 120})
        self.assertEqual((execution["caller_deadline_unix_ms"], execution["deadline_unix_ms"]),
                         (None, execution["run"]["deadline_unix_ms"]))
        self.assertEqual(execution["ended_unix_ms"], 1800000000000)
        # A caller's deadline only tightens what the build may use; the run's stays frozen.
        _, tightened = self.build(run_id="tightened", wall=lambda: 1800000000.0,
                                  deadline_unix_ms=1800000600000)
        self.assertEqual(tightened["execution"]["run"], execution["run"])
        self.assertEqual(tightened["execution"]["deadline_unix_ms"], 1800000600000)
        with self.assertRaises(builds.Unavailable) as caught:
            self.build(run_id="late", wall=lambda: 1800000000.0,
                       deadline_unix_ms=1800000000000)
        self.assertEqual(caught.exception.reason, "run_deadline")
        self.assertFalse(os.path.exists(os.path.join(self.repo.path, "target", "performance",
                                                     "late")))

    def test_a_frozen_plan_builds_each_product_once_with_verified_provenance(self):
        builder, doc = self.build()
        layout = builder.layout
        self.assertEqual(layout.run, os.path.join(os.path.realpath(self.repo.path), "target",
                                                  "performance", "fixture_build_run"))
        self.assertEqual(doc["provenance"], "measured")
        self.assertEqual(doc["toolchain"]["rustc"], "rustc 1.88.0 (fixture 2025-06-23)")
        self.assertEqual(doc["linker"]["identity"], "@(#)PROGRAM:ld PROJECT:fixture")
        self.assertTrue(doc["isolation"]["credential_free_git"])
        # The supervisor once, from the authority; each product once, from its own source.
        self.assertEqual(doc["supervisor"]["source_sha"], self.authority)
        self.assertEqual(doc["supervisor"]["status"], "succeeded")
        self.assertIn("{run}/worktrees/authority/target", doc["supervisor"]["step"]["argv"])
        base, cand = doc["sides"]["baseline"], doc["sides"]["candidate"]
        self.assertIn("{run}/worktrees/base/target", base["argv"])
        self.assertIn("{run}/worktrees/head/target", cand["argv"])
        for side, sha in ((base, self.base), (cand, self.head)):
            self.assertEqual(side["status"], "succeeded", side)
            self.assertEqual(side["embedded_commit"], {"expected": sha[:12], "verified": True,
                                                       "method": "binary_bytes"})
            self.assertTrue(side["source_unchanged"])
            self.assertEqual(side["environment"]["HOME"], "{run}/home")
            self.assertEqual(side["environment"]["CARGO_NET_OFFLINE"], "true")
            self.assertNotIn("GITHUB_TOKEN", side["environment"])
            self.assertIn("--locked", side["argv"])
        # Adapters in priority order: the baseline lacks the newer API, so its first adapter
        # fails to compile and the second is chosen; the candidate's first compiles.
        self.assertEqual([(item["adapter_id"], item["outcome"])
                          for item in base["adapter_outcomes"]],
                         [("fixture_probe_v2", "failed"), ("fixture_probe_v1", "compiled")])
        self.assertEqual([(item["adapter_id"], item["outcome"])
                          for item in cand["adapter_outcomes"]],
                         [("fixture_probe_v2", "compiled"), ("fixture_probe_v1",
                                                             "not_attempted")])
        self.assertEqual(doc["adapter_binding"], {
            "baseline": "fixture_probe_v1", "candidate": "fixture_probe_v2",
            "baseline_group": "fixture_group_v1", "candidate_group": "fixture_group_v2",
            "equivalent": False})
        # Built artifacts are verified before any use, and nothing else of the run remains.
        verified, paths = builds.verify_artifacts(layout.build)
        self.assertEqual(verified, doc)
        self.assertEqual(sorted(os.listdir(layout.run)), ["build"])
        self.assertEqual(doc["cleanup"]["status"], "removed")
        self.assertEqual(sorted(os.listdir(layout.build)), ["artifacts", "builds.json", "logs",
                                                           "run.json"])
        self.assertIn(b"commit=" + self.head[:12].encode(), read(paths["candidate/af"]))

    def test_candidate_edits_to_its_probe_copy_never_reach_the_instrument(self):
        builder, doc = self.build()
        _, paths = builds.verify_artifacts(builder.layout.build)
        probe = read(paths["candidate/probe"])
        self.assertIn(b"authority probe v2", probe)
        self.assertIn(b"authority helper module", probe)
        self.assertNotIn(b"CANDIDATE-EDITED", probe)
        overlay = doc["sides"]["candidate"]["overlay"]
        for item in overlay["files"]:
            committed = self.repo.git("show", "%s:%s" % (self.authority, item["path"]))
            self.assertEqual(item["sha256"], sha256_bytes(committed.encode("utf-8") + b"\n"))
        self.assertEqual([item["path"] for item in overlay["files"]],
                         ["crates/review-pipeline/examples/perf-probe.rs",
                          "crates/review-pipeline/examples/perf-probe/helper.rs"])
        # The failed adapter's overlay was restored: the baseline probe is the older bridge.
        self.assertIn(b"older public API", read(paths["baseline/probe"]))

    def test_one_commit_on_both_sides_builds_once_and_shares_its_artifacts(self):
        builder, doc = self.build(baseline=self.head, candidate=self.head, run_id="same_source")
        base, cand = doc["sides"]["baseline"], doc["sides"]["candidate"]
        self.assertEqual(cand["shared_with"], "baseline")
        self.assertEqual(cand["binaries"], base["binaries"])
        self.assertEqual(cand["artifacts"], base["artifacts"])
        self.assertEqual(sorted(name for name in os.listdir(builder.layout.logs)
                                if name.startswith("candidate")), [])
        self.assertTrue(doc["adapter_binding"]["equivalent"])

    def test_a_source_no_adapter_compiles_against_has_unavailable_coverage(self):
        old = self.repo.commit("history", {"crates/review-pipeline/src/lib.rs":
                                           "pub fn unrelated() {}\n"})
        authority = self.repo.commit("authority again", {"README": "authority\n"})
        self.authority = authority
        _, doc = self.build(baseline=old, run_id="unsupported_baseline")
        base = doc["sides"]["baseline"]
        self.assertEqual([item["outcome"] for item in base["adapter_outcomes"]],
                         ["failed", "failed"])
        self.assertIsNone(base["chosen_adapter"])
        self.assertEqual(base["probe_coverage"], "unavailable")
        self.assertIn("no declared adapter compiles", base["coverage_reason"])
        self.assertEqual(base["status"], "succeeded")
        self.assertFalse(doc["adapter_binding"]["equivalent"])

    def test_failed_builds_are_recorded_with_their_evidence_never_substituted(self):
        cases = (("COMPILE_ERROR", "af does not compile"),
                 ("NO_COMMIT", None), ("TOUCH_SOURCE", None))
        for index, (marker, logged) in enumerate(cases):
            with self.subTest(marker=marker):
                head = self.repo.commit(marker, {"crates/af/src/main.rs":
                                                 "fn main() {} // %s\n" % marker})
                builder, doc = self.build(candidate=head, run_id="failed_%d" % index)
                cand = doc["sides"]["candidate"]
                self.assertEqual(cand["status"], "failed")
                self.assertEqual(doc["sides"]["baseline"]["status"], "succeeded")
                if marker == "COMPILE_ERROR":
                    self.assertIsNone(cand["binaries"]["af"])
                    self.assertEqual(cand["product"]["status"], "failed")
                    self.assertIn(logged.encode(), read(os.path.join(
                        builder.layout.logs, "candidate-af.log")))
                elif marker == "NO_COMMIT":
                    self.assertFalse(cand["embedded_commit"]["verified"])
                else:
                    self.assertFalse(cand["source_unchanged"])
                self.assertTrue(os.path.isfile(os.path.join(builder.layout.build,
                                                            "builds.json")))

    def test_a_hostile_build_output_is_never_read_outside_its_target_or_published(self):
        # Root-reproduced (F6): a candidate output symlinked to an owned file outside its
        # target directory had that file's bytes copied into the retained artifacts.
        marker = self.path("outside-marker")
        with open(marker, "wb") as stream:
            stream.write(b"FIXTURE-AF commit=%s owned marker bytes\n" % self.head[:12].encode())
        os.chmod(marker, 0o600)
        elsewhere = self.path("elsewhere")
        os.mkdir(elsewhere, 0o700)
        with open(os.path.join(elsewhere, "af"), "wb") as stream:
            stream.write(read(marker))
        cases = (("SYMLINK_OUTPUT:" + marker, "is a symlink"),
                 ("SYMLINK_RELEASE:" + elsewhere, "release is not a directory"),
                 ("SPARSE_OUTPUT", "logical bytes; "))
        for index, (marker_text, refused) in enumerate(cases):
            with self.subTest(case=marker_text.split(":")[0]):
                head = self.repo.commit("hostile %d" % index, {
                    "crates/af/src/main.rs": "fn main() {} // %s\n" % marker_text})
                reads = []
                original = builds.admission.read_bounded

                def counted(fd, size, budget, what, step, sink=None):
                    reads.append(what)
                    return original(fd, size, budget, what, step, sink)

                with mock.patch.object(builds.admission, "read_bounded", counted):
                    builder, doc = self.build(candidate=head, run_id="hostile_%d" % index)
                cand = doc["sides"]["candidate"]
                self.assertEqual(cand["status"], "failed")
                self.assertEqual(doc["sides"]["baseline"]["status"], "succeeded")
                self.assertEqual(cand["product"]["status"], "succeeded")
                self.assertIsNone(cand["artifacts"]["af"])
                self.assertIsNone(cand["binaries"]["af"])
                self.assertFalse(cand["embedded_commit"]["verified"])
                # The candidate's own AF output was never read; only admitted outputs were.
                self.assertNotIn("the candidate output target/%s/release/af"
                                 % builder.recipe["target"], reads)
                self.assertFalse(os.path.lexists(os.path.join(builder.layout.artifacts,
                                                              "candidate", "af")))
                for current, _, names in os.walk(builder.layout.artifacts):
                    for name in names:
                        self.assertNotIn(b"owned marker bytes",
                                         read(os.path.join(current, name)))
                # The refusal is recorded in the step's retained log, which builds.json binds.
                log = read(os.path.join(builder.layout.logs, "candidate-af.log"))
                self.assertIn(b"perflab: the candidate output", log)
                self.assertIn(refused.encode(), log)
                self.assertEqual(cand["product"]["log"]["sha256"], sha256_bytes(log))
                self.assertTrue(read(marker).endswith(b" owned marker bytes\n"))
                self.assertEqual(os.stat(marker).st_mode & 0o777, 0o600)
                # The baseline stays usable evidence; the verdict sees an unusable candidate.
                builds.verify_artifacts(builder.layout.build)

    def test_a_refused_probe_output_fails_its_side_and_tries_no_other_adapter(self):
        marker = self.path("outside-probe")
        with open(marker, "wb") as stream:
            stream.write(b"owned probe bytes outside the target\n")
        os.chmod(marker, 0o600)
        head = self.repo.commit("hostile probe", {
            "crates/review-pipeline/src/lib.rs":
                "pub fn answer() -> u32 { 42 }\npub fn answer_v2() -> u32 { 43 }\n"
                "// SYMLINK_EXAMPLE:%s\n" % marker})
        authority = self.repo.commit("authority over the hostile probe",
                                     {"README": "authority\n"})
        plan = plan_for(self.repo, self.base, head, authority, run_id="hostile_probe")
        builder = builds.Builder(self.repo.path, plan, self.toolchain, free_bytes=PLENTY)
        doc = builder.build()
        cand = doc["sides"]["candidate"]
        self.assertEqual((cand["status"], cand["probe_coverage"]), ("failed", "unavailable"))
        self.assertIn("is a symlink", cand["coverage_reason"])
        self.assertEqual(cand["chosen_adapter"], "fixture_probe_v2")
        self.assertEqual([item["outcome"] for item in cand["adapter_outcomes"]],
                         ["compiled", "not_attempted"])
        self.assertIsNone(cand["artifacts"]["probe"])
        self.assertIsNotNone(cand["artifacts"]["af"])
        self.assertEqual(doc["sides"]["baseline"]["status"], "succeeded")
        self.assertFalse(os.path.lexists(os.path.join(builder.layout.artifacts, "candidate",
                                                      "perf-probe")))
        log = read(os.path.join(builder.layout.logs, "candidate-probe-fixture_probe_v2.log"))
        self.assertIn(b"perflab: the candidate output", log)
        self.assertEqual(read(marker), b"owned probe bytes outside the target\n")

    def test_a_build_script_rewriting_the_overlaid_closure_refuses_the_build(self):
        # The candidate's own build script rewrites the authority helper it is compiled with.
        # Whether that compile then succeeds or fails, the build refuses with the integrity
        # failure: no probe from that source is published, no later adapter is tried, the
        # compile log stays as evidence and no builds.json claims an unchanged source.
        for index, marker in enumerate(("MUTATE_OVERLAY", "MUTATE_OVERLAY_AND_FAIL")):
            with self.subTest(marker=marker):
                head = self.repo.commit(marker, {
                    "crates/review-pipeline/src/lib.rs":
                        "pub fn answer() -> u32 { 42 }\npub fn answer_v2() -> u32 { 43 }\n"
                        "// %s\n" % marker})
                authority = self.repo.commit("authority %d" % index, {
                    "README": "authority %d\n" % index})
                plan = plan_for(self.repo, self.base, head, authority,
                                run_id="mutated_%d" % index)
                builder = builds.Builder(self.repo.path, plan, self.toolchain,
                                         free_bytes=PLENTY)
                with self.assertRaises(builds.OverlayIntegrity) as raised:
                    builder.build()
                self.assertEqual(raised.exception.reason, "overlay_integrity")
                detail = str(raised.exception)
                self.assertIn("crates/review-pipeline/examples/perf-probe/helper.rs", detail)
                self.assertIn("during candidate's fixture_probe_v2 compile", detail)
                logs = sorted(os.listdir(builder.layout.logs))
                self.assertIn("candidate-probe-fixture_probe_v2.log", logs)
                self.assertNotIn("candidate-probe-fixture_probe_v1.log", logs)
                self.assertFalse(os.path.exists(os.path.join(builder.layout.build,
                                                             "builds.json")))
                self.assertFalse(os.path.exists(os.path.join(builder.layout.artifacts,
                                                             "candidate", "perf-probe")))

    def test_the_overlay_verifies_type_mode_and_bytes_and_restores_exactly(self):
        root = self.path("overlay")
        os.makedirs(os.path.join(root, "examples"))
        original = os.path.join(root, "examples", "probe.rs")
        with open(original, "wb") as stream:
            stream.write(b"the measured source's own probe\n")
        os.chmod(original, 0o640)
        overlay = builds.Overlay(root, [("examples/probe.rs", b"authority\n"),
                                        ("examples/probe/helper.rs", b"helper\n")])
        overlay.apply()
        overlay.verify("after apply")
        self.assertEqual(os.stat(original).st_mode & 0o777, builds.OVERLAY_MODE)
        helper = os.path.join(root, "examples", "probe", "helper.rs")
        os.unlink(helper)
        os.symlink(original, helper)
        self.assertIn("examples/probe/helper.rs is no longer a regular file",
                      "; ".join(overlay.problems()))
        os.unlink(helper)
        with open(helper, "wb") as stream:
            stream.write(b"helper\n")
        self.assertIn("has mode", "; ".join(overlay.problems()))
        os.chmod(helper, builds.OVERLAY_MODE)
        self.assertEqual(overlay.problems(), [])
        overlay.restore()
        self.assertEqual(read(original), b"the measured source's own probe\n")
        self.assertEqual(os.stat(original).st_mode & 0o777, 0o640)
        self.assertFalse(os.path.exists(os.path.join(root, "examples", "probe")))

    def test_tampered_artifacts_and_manifests_are_refused_before_launch(self):
        builder, doc = self.build()
        _, paths = builds.verify_artifacts(builder.layout.build)
        target = paths["candidate/af"]
        os.chmod(target, 0o755)
        with open(target, "ab") as stream:
            stream.write(b"tampered")
        with self.assertRaises(ContractError):
            builds.verify_artifacts(builder.layout.build)
        with self.assertRaises(ContractError):
            builds.verified_binary(target, doc["sides"]["candidate"]["binaries"]["af"])
        builds_path = os.path.join(builder.layout.build, "builds.json")
        edited = json.loads(read(builds_path))
        edited["sides"]["baseline"]["binaries"]["af"] = edited["supervisor"]["binary_sha256"]
        with open(builds_path, "w") as stream:
            stream.write(json.dumps(edited))
        with self.assertRaises(ContractError):
            builds.verify_artifacts(builder.layout.build)

    def test_the_authority_commit_supplies_the_adapters_and_their_complete_closure(self):
        # The default registry is the authority commit's own, checked against its tree.
        _, doc = self.build(run_id="authority_registry")
        self.assertEqual(doc["sides"]["candidate"]["chosen_adapter"], "fixture_probe_v2")
        broken = self.repo.commit("drop helper", {
            "fixtures/performance/adapters.toml": build_fixtures.ADAPTERS.replace(
                ', "crates/review-pipeline/examples/perf-probe/helper.rs"', "")})
        self.authority = broken
        with self.assertRaises(ContractError) as caught:
            self.build(run_id="incomplete_closure")
        self.assertIn("perf-probe/helper.rs", str(caught.exception))
        self.authority = self.base
        with self.assertRaises(builds.Unavailable) as caught:
            self.build(run_id="no_registry")
        self.assertEqual(caught.exception.reason, "no_adapter_registry")

    def test_the_private_repository_carries_no_checkout_credentials(self):
        self.repo.git("config", "http.https://example.invalid/.extraheader",
                      "AUTHORIZATION: basic c2VjcmV0")
        self.repo.git("config", "credential.helper", "store")
        builder, doc = self.build(keep_worktrees=True)
        self.assertTrue(doc["isolation"]["credential_free_git"])
        env = builds.git_environment(builder.layout.home)
        listing = subprocess.run(["git", "-C", builder.layout.worktree("head"), "config",
                                  "--list"], env=env, stdout=subprocess.PIPE).stdout
        self.assertNotIn(b"extraheader", listing)
        self.assertNotIn(b"credential", listing)
        self.assertEqual(doc["cleanup"]["status"], "retained")
        gitfile = read(os.path.join(builder.layout.worktree("head"), ".git"))
        self.assertTrue(gitfile.startswith(b"gitdir: " + builder.layout.git.encode()))

    def test_preconditions_refuse_before_anything_is_created(self):
        plan = plan_for(self.repo, self.base, self.head, self.authority)
        run = os.path.join(self.repo.path, "target", "performance", plan["run_id"])
        with self.assertRaises(builds.Unavailable) as caught:
            builds.Toolchain(self.toolchain.bin_dir, "1.89.0")
        self.assertEqual(caught.exception.reason, "toolchain_mismatch")
        linux = builds.Toolchain(fake_toolchain(self.path("linux-toolchain"),
                                                "x86_64-unknown-linux-gnu"), "1.88.0")
        for toolchain, free, reason in ((linux, PLENTY, "host_mismatch"),
                                        (self.toolchain, lambda path: 1, "disk_floor")):
            with self.subTest(reason=reason):
                builder = builds.Builder(self.repo.path, plan, toolchain, registry(),
                                         free_bytes=free)
                with self.assertRaises(builds.Unavailable) as caught:
                    builder.build()
                self.assertEqual(caught.exception.reason, reason)
                self.assertFalse(os.path.exists(run))
        os.makedirs(os.path.join(self.repo.path, "target", ".cargo"))
        with open(os.path.join(self.repo.path, "target", ".cargo", "config.toml"), "w") as s:
            s.write('[build]\nrustflags = ["-Copt-level=0"]\n')
        builder = builds.Builder(self.repo.path, plan, self.toolchain, registry(),
                                 free_bytes=PLENTY)
        with self.assertRaises(builds.Unavailable) as caught:
            builder.build()
        self.assertEqual(caught.exception.reason, "ambient_cargo_config")
        self.assertFalse(os.path.exists(run))
        with open(os.path.join(self.repo.path, "target", ".cargo", "config.toml"), "w") as s:
            s.write('[build]\ntarget-dir = "/elsewhere"\n')
        builder, doc = self.build()
        self.assertIn("overridden by --target-dir", doc["isolation"]["detail"])
        with self.assertRaises(ContractError):
            self.build()

    def test_the_shipping_recipe_builds_through_exactly_its_pinned_cargo_zigbuild(self):
        def shipping(recipe):
            recipe.update(os="linux", target="x86_64-unknown-linux-musl",
                          builder="cargo_zigbuild", cargo_zigbuild="0.23.4", zig="0.15.2",
                          linker="zig_cc")
        linux = builds.Toolchain(build_fixtures.fake_toolchain(
            self.path("linux-toolchain"), "aarch64-unknown-linux-gnu"), "1.88.0")
        plan = plan_for(self.repo, self.base, self.head, self.authority,
                        run_id="shipping", recipe_edit=shipping, trusted_host_build=True)
        builder = builds.Builder(self.repo.path, plan, linux, free_bytes=PLENTY,
                                 zigbuild_bin=build_fixtures.fake_zigbuild(self.path("zig")))
        doc = builder.build()
        # Explicitly trusted host mode claims no filesystem isolation.
        self.assertEqual((doc["isolation"]["kind"], doc["isolation"]["filesystem"],
                          doc["isolation"]["compile_network"], doc["isolation"]["image"]),
                         ("host_process", "trusted_host", "host", None))
        self.assertIn("explicitly trusted host build", doc["isolation"]["detail"])
        for side in ("baseline", "candidate"):
            self.assertEqual(doc["sides"][side]["status"], "succeeded", doc["sides"][side])
            self.assertEqual(doc["sides"][side]["argv"][1], "zigbuild")
            self.assertIn("--locked", doc["sides"][side]["argv"])
            self.assertEqual(doc["sides"][side]["settings"]["builder"], "cargo_zigbuild")
        self.assertEqual(doc["supervisor"]["step"]["argv"][1], "zigbuild")
        self.assertEqual(doc["linker"], {"kind": "zig_cc", "identity": "0.15.2",
                                         "unavailable": None})
        self.assertIn("cargo-zigbuild 0.23.4; zig 0.15.2", doc["isolation"]["detail"])
        _, paths = builds.verify_artifacts(builder.layout.build)
        self.assertIn(b"FIXTURE-AF commit=" + self.head[:12].encode(),
                      read(paths["candidate/af"]))
        # Another version of either tool is refused before anything is created.
        for index, (tool, version) in enumerate((("cargo_zigbuild", "0.23.5"),
                                                 ("zig", "0.15.1"))):
            with self.subTest(tool=tool):
                options = {"cargo_zigbuild": "0.23.4", "zig": "0.15.2"}
                options[tool] = version
                zig = build_fixtures.fake_zigbuild(self.path("zig-%d" % index), **options)
                other = plan_for(self.repo, self.base, self.head, self.authority,
                                 run_id="shipping_%d" % index, recipe_edit=shipping,
                                 trusted_host_build=True)
                builder = builds.Builder(self.repo.path, other, linux, free_bytes=PLENTY,
                                         zigbuild_bin=zig)
                with self.assertRaises(builds.Unavailable) as caught:
                    builder.build()
                self.assertEqual(caught.exception.reason, "toolchain_mismatch")
                self.assertFalse(os.path.exists(builder.layout.run))

    def test_a_diagnostic_recipe_builds_its_features_symbols_and_frame_pointers(self):
        # ADR-0174: only a diagnostic plan builds an instrumented recipe, and its features
        # reach the probe as well as af; frame pointers and symbols reach every compile.
        loaded = suite_module.load(SUITE)
        declared = diagnostics.load(os.path.join(ROOT, diagnostics.REGISTRY_PATH), loaded.doc)
        for recipe_id, features in (("macos_arm64_trace", ["perf-trace"]),
                                    ("macos_arm64_samply", [])):
            recipe = diagcontracts.diagnostic_recipe(loaded.doc, declared, recipe_id)
            plan = plan_for(self.repo, self.base, self.head, self.authority,
                            run_id="diag_" + recipe_id, diagnostic_recipe=(
                                recipe_id, recipe,
                                diagcontracts.frozen_identity(declared, recipe_id)))
            self.assertEqual((plan["scope"], plan["profile"]["recipe_id"]),
                             ("diagnostic", recipe_id))
            builder = builds.Builder(self.repo.path, plan, self.toolchain, registry(),
                                     free_bytes=PLENTY)
            doc = builder.build()
            # The child run takes its probes and its supervisor from this verified build only.
            probes, supervisor, manifest = diagnostics._probes(builder.layout.build, plan,
                                                               time.monotonic() + 60)
            self.assertEqual(sorted(probes), ["baseline", "candidate"])
            self.assertEqual(supervisor[1], doc["supervisor"]["artifact"]["sha256"])
            self.assertTrue(os.path.isfile(supervisor[0]))
            for side, (path, digest) in probes.items():
                self.assertEqual(digest, doc["sides"][side]["artifacts"]["probe"]["sha256"])
                self.assertTrue(os.path.isfile(path))
            self.assertTrue(manifest.startswith("sha256:"))
            for side in ("baseline", "candidate"):
                record = doc["sides"][side]
                self.assertEqual(record["status"], "succeeded", record)
                probe = [item for item in record["adapter_outcomes"]
                         if item["outcome"] == "compiled"][0]["step"]
                for argv in (record["argv"], probe["argv"]):
                    if features:
                        self.assertEqual(argv[argv.index("--features") + 1],
                                         ",".join(features))
                    else:
                        self.assertNotIn("--features", argv)
                environment = record["environment"]
                if recipe["frame_pointers"]:
                    self.assertIn("-Cforce-frame-pointers=yes",
                                  environment["CARGO_ENCODED_RUSTFLAGS"])
                    self.assertEqual(environment["CARGO_PROFILE_RELEASE_DEBUG"], "true")
                else:
                    self.assertNotIn("CARGO_ENCODED_RUSTFLAGS", environment)

    def test_a_recipe_this_host_cannot_build_is_unavailable(self):
        def zig(recipe):
            recipe.update(builder="cargo_zigbuild", cargo_zigbuild="0.23.4", zig="0.15.2",
                          linker="zig_cc")
        for edit, reason in ((zig, "builder_unavailable"),
                             (lambda recipe: recipe.update(instrumented=True),
                              "instrumented_recipe")):
            plan = plan_for(self.repo, self.base, self.head, self.authority, recipe_edit=edit)
            builder = builds.Builder(self.repo.path, plan, self.toolchain, registry(),
                                     free_bytes=PLENTY)
            with self.assertRaises(builds.Unavailable) as caught:
                builder.build()
            self.assertEqual(caught.exception.reason, reason)

    def test_gnu_cpu_settings_are_frozen_recorded_and_diagnostic_only(self):
        loaded = suite_module.load(SUITE)
        declared = diagnostics.load(os.path.join(ROOT, diagnostics.REGISTRY_PATH), loaded.doc)
        recipe_id = "linux_gnu_cpu_profile"
        recipe = diagcontracts.diagnostic_recipe(loaded.doc, declared, recipe_id)
        toolchain = builds.Toolchain(fake_toolchain(self.path("linux-toolchain"),
                                                   "x86_64-unknown-linux-gnu"), "1.88.0")
        for shared in (False, True):
            plan = plan_for(self.repo, self.base, self.base if shared else self.head,
                            self.authority, run_id="gnu_cpu_%s" % str(shared).lower(), tier="quick",
                            environment="linux_x86_64_gnu_hosted", trusted_host_build=True,
                            diagnostic_recipe=(recipe_id, recipe,
                                diagcontracts.frozen_identity(declared, recipe_id)))
            builder = builds.Builder(self.repo.path, plan, toolchain, registry(),
                                     free_bytes=PLENTY)
            doc = builder.build()
            verified, paths = builds.verify_artifacts(builder.layout.build)
            self.assertEqual(verified, doc)
            self.assertEqual(doc["recipe_digest"], contracts.recipe_digest(
                recipe_id, recipe, plan["profile"]["build"]))
            records = [doc["supervisor"]]
            total = doc["supervisor"]["artifact"]["size"]
            for side in ("baseline", "candidate"):
                record = doc["sides"][side]
                self.assertEqual(record["status"], "succeeded")
                self.assertEqual(record["settings"], contracts.recipe_settings(recipe))
                records.append(record["product"])
                records.extend(item["step"] for item in record["adapter_outcomes"]
                               if item["step"] is not None)
                for kind in ("af", "probe"):
                    artifact = record["artifacts"][kind]
                    data = read(paths[side + "/" + kind])
                    self.assertEqual((artifact["size"], artifact["sha256"]),
                                     (len(data), sha256_bytes(data)))
                    if side == "baseline" or not shared:
                        total += len(data)
            for record in records:
                env = record.get("environment", record.get("step", {}).get("environment"))
                self.assertEqual(env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f"),
                                 recipe["rustflags"] + ["-Cforce-frame-pointers=yes"])
                self.assertEqual(env["CARGO_PROFILE_RELEASE_DEBUG"], "true")
            # Manifest overhead is also charged by the build's final accounting.
            self.assertGreaterEqual(doc["artifacts"]["bytes"], total)
            self.assertLessEqual(doc["artifacts"]["bytes"], doc["artifacts"]["limit_bytes"])
            self.assertEqual(doc["sides"]["candidate"]["shared_with"],
                             "baseline" if shared else None)
        ordinary = plan_for(self.repo, self.base, self.head, self.authority, tier="quick",
                            environment="linux_x86_64_gnu_hosted", trusted_host_build=True)
        builder = builds.Builder(self.repo.path, ordinary, toolchain, registry(),
                                 free_bytes=PLENTY)
        self.assertEqual(builder.recipe["rustflags"], [])
        self.assertNotIn("CARGO_PROFILE_RELEASE_DEBUG", builder.environment())
        self.assertNotIn("CARGO_ENCODED_RUSTFLAGS", builder.environment())

    def test_the_musl_shipping_cpu_profile_builds_bounded_line_tables_with_frame_pointers(self):
        # The Claw musl CPU acceptance built a full-debug 802,606,040-byte baseline AF that
        # left no room under the unchanged 1-GiB artifact cap for the later artifacts. Its
        # diagnostic build now freezes the same line-tables-only level as the GNU recipe,
        # through cargo-zigbuild, recorded in every step and the recipe digest; the shipping
        # verdict build keeps neither.
        loaded = suite_module.load(SUITE)
        declared = diagnostics.load(os.path.join(ROOT, diagnostics.REGISTRY_PATH), loaded.doc)
        recipe_id = "linux_musl_cpu_profile"
        recipe = diagcontracts.diagnostic_recipe(loaded.doc, declared, recipe_id)
        base = loaded.doc["recipes"]["linux_musl_shipping"]
        self.assertEqual((recipe["builder"], recipe["linker"], recipe["target"]),
                         ("cargo_zigbuild", "zig_cc", "x86_64-unknown-linux-musl"))
        self.assertEqual(recipe["rustflags"], ["-Cdebuginfo=line-tables-only"])
        self.assertEqual(base["rustflags"], [])
        self.assertTrue(recipe["debug_symbols"] and recipe["frame_pointers"])
        toolchain = builds.Toolchain(build_fixtures.fake_toolchain(
            self.path("musl-toolchain"), "x86_64-unknown-linux-musl"), "1.88.0")
        zig = build_fixtures.fake_zigbuild(self.path("zig"))
        plan = plan_for(self.repo, self.base, self.head, self.authority, run_id="musl_cpu",
                        environment="linux_x86_64_musl_hosted", trusted_host_build=True,
                        diagnostic_recipe=(recipe_id, recipe,
                                           diagcontracts.frozen_identity(declared, recipe_id)))
        builder = builds.Builder(self.repo.path, plan, toolchain, registry(),
                                 free_bytes=PLENTY, zigbuild_bin=zig)
        doc = builder.build()
        self.assertEqual(doc["recipe_digest"], contracts.recipe_digest(
            recipe_id, recipe, plan["profile"]["build"]))
        records = [doc["supervisor"]["step"]]
        for side in ("baseline", "candidate"):
            record = doc["sides"][side]
            self.assertEqual(record["status"], "succeeded", record)
            self.assertEqual(record["argv"][1], "zigbuild")
            self.assertEqual(record["settings"], contracts.recipe_settings(recipe))
            records.append(record)
            records.extend(item["step"] for item in record["adapter_outcomes"]
                           if item["step"] is not None)
        for record in records:
            env = record["environment"]
            self.assertEqual(env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f"),
                             ["-Cdebuginfo=line-tables-only", "-Cforce-frame-pointers=yes"])
            self.assertEqual(env["CARGO_PROFILE_RELEASE_DEBUG"], "true")
        shipping = plan_for(self.repo, self.base, self.head, self.authority,
                            run_id="musl_shipping", environment="linux_x86_64_musl_hosted",
                            trusted_host_build=True)
        builder = builds.Builder(self.repo.path, shipping, toolchain, registry(),
                                 free_bytes=PLENTY, zigbuild_bin=zig)
        self.assertEqual(builder.recipe["rustflags"], [])
        self.assertNotIn("CARGO_PROFILE_RELEASE_DEBUG", builder.environment())
        self.assertNotIn("CARGO_ENCODED_RUSTFLAGS", builder.environment())

    def test_the_reported_production_probe_size_is_refused_before_reading(self):
        # #353's actual failure: AF and the supervisor had consumed this room before the
        # 531-MB full-debug probe arrived. Keep that refusal under the unchanged cap.
        plan = plan_for(self.repo, self.base, self.head, self.authority)
        builder = builds.Builder(self.repo.path, plan, self.toolchain, registry(),
                                 free_bytes=PLENTY)
        builder.layout.create()
        builder.published = 799043896 + 14749752
        output = self.path("production-probe")
        with open(output, "wb") as stream:
            stream.truncate(531252216)
        with mock.patch.object(builds.admission, "read_bounded") as reading:
            artifact, _, refusal = builder._publish(self.tmp, "production-probe",
                                                    "baseline", "perf-probe")
        self.assertIsNone(artifact)
        self.assertIn("259948176 remain", refusal)
        self.assertEqual(builder.budget.artifact_limit, 1073741824)
        reading.assert_not_called()


IMAGE = "sha256:" + "a" * 64
OTHER_IMAGE = "sha256:" + "b" * 64
# A repository manifest digest: the local image it names has its own, different image ID.
NAMED = "registry.example/af-build@sha256:" + "e" * 64
NAMED_ID = "sha256:" + "f" * 64
# The same manifest digest under another repository name: not the frozen image.
RENAMED = "registry.example/other@sha256:" + "e" * 64


class ImageIdentity(unittest.TestCase):
    """`builds.image_identity` reads the five `docker image inspect` identity fields, each of
    its shape, or refuses them as `build_image_malformed`: never a JSON or type error."""

    GOOD = ['"%s"' % IMAGE, '"linux"', '"arm64"', '["%s"]' % NAMED, '["A=1", "B"]']

    def identity(self, fields):
        return builds.image_identity(("\t".join(fields) + "\n").encode(), IMAGE, "build image")

    def test_well_formed_identity_fields_are_returned_with_their_shapes(self):
        self.assertEqual(self.identity(self.GOOD),
                         [IMAGE, "linux", "arm64", [NAMED], ["A=1", "B"]])
        self.assertEqual(self.identity(self.GOOD[:3] + ["null", "null"])[3:], [None, None])

    def test_malformed_or_misshapen_identity_fields_are_a_typed_refusal(self):
        good = self.GOOD
        bad = [(good[:field] + ['"unterminated'] + good[field + 1:], "is not JSON")
               for field in range(5)]
        bad += [(["null"] + good[1:], "ID is not a non-empty string"),
                (good[:1] + ['""'] + good[2:], "OS is not a non-empty string"),
                (good[:2] + ["7"] + good[3:], "architecture is not a non-empty string"),
                (good[:3] + ['"%s"' % NAMED, good[4]], "repository digests is not a list"),
                (good[:3] + ['[1]', good[4]], "repository digests is not a list"),
                (good[:4] + ['{"A": "1"}'], "environment is not a list"),
                (good[:4] + ['["A=1", null]'], "environment is not a list"),
                (good[:4], "4 fields, not 5"), (good + ["null"], "6 fields, not 5"),
                ([], "1 fields, not 5")]
        for fields, why in bad:
            with self.subTest(fields=fields):
                with self.assertRaises(builds.Unavailable) as caught:
                    self.identity(fields)
                self.assertEqual(caught.exception.reason, "build_image_malformed")
                self.assertIn("malformed identity for the build image %s" % IMAGE,
                              str(caught.exception))
                self.assertIn(why, str(caught.exception))
        with self.assertRaises(builds.Unavailable) as caught:
            builds.image_identity(b"x" * (builds.limits.RECORD_BYTES_MAX + 1), IMAGE,
                                  "build image")
        self.assertIn("exceeds its byte bound", str(caught.exception))

    def test_deep_nesting_below_the_byte_bound_is_a_typed_refusal(self):
        # 1200 nested arrays fit the byte bound but can recurse the decoder past the
        # interpreter's limit: the depth is refused before any field is decoded.
        nested = "[" * 1200 + "]" * 1200
        loads = json.loads

        def decoded(text):
            assert text != nested, "decoded the nested field"
            return loads(text)
        for field in range(5):
            fields = self.GOOD[:field] + [nested] + self.GOOD[field + 1:]
            stdout = ("\t".join(fields) + "\n").encode()
            self.assertLessEqual(len(stdout), builds.limits.RECORD_BYTES_MAX)
            with self.subTest(field=field):
                with mock.patch.object(builds.json, "loads", side_effect=decoded):
                    with self.assertRaises(builds.Unavailable) as caught:
                        builds.image_identity(stdout, IMAGE, "build image")
                self.assertEqual(caught.exception.reason, "build_image_malformed")
                self.assertIn("nests deeper than 1", str(caught.exception))
        # Brackets inside a string are its contents, not nesting.
        self.assertEqual(self.identity(self.GOOD[:4] + [r'["A=[[{\"]]", "B=\\"]'])[4],
                         ['A=[[{"]]', "B=\\"])
        # A decoder that still recurses too deeply is the same refusal, never a RecursionError.
        with mock.patch.object(builds.json, "loads", side_effect=RecursionError("deep")):
            with self.assertRaises(builds.Unavailable) as caught:
                self.identity(self.GOOD)
        self.assertEqual(caught.exception.reason, "build_image_malformed")
        self.assertIn("its ID is not JSON", str(caught.exception))

    def test_malformed_utf8_is_a_typed_refusal_never_repaired(self):
        # An environment PATH holding byte 0xff is refused, not replaced by U+FFFD and admitted.
        fields = [field.encode() for field in self.GOOD[:4]] + [b'["PATH=/usr/\xffbin"]']
        with self.assertRaises(builds.Unavailable) as caught:
            builds.image_identity(b"\t".join(fields) + b"\n", IMAGE, "build image")
        self.assertEqual(caught.exception.reason, "build_image_malformed")
        self.assertIn("it is not UTF-8", str(caught.exception))
        # Well-formed non-ASCII UTF-8 is still read exactly.
        self.assertEqual(self.identity(self.GOOD[:4] + ['["LANG=caf\u00e9"]'])[4],
                         ["LANG=caf\u00e9"])


class ContainerBuild(TempDir):
    """A Linux recipe builds in its pinned build image (ADR-0169). The stand-in docker client
    of `tests.build_fixtures` executes each container's entrypoint on this host with exactly
    the variables it was given and logs every invocation, so the tests read what each container
    was granted: its network, mounts, user and environment."""

    def setUp(self):
        TempDir.setUp(self)
        self.repo, self.base, self.head, self.authority = fixture_repo(self.path("checkout"))
        bin_dir = fake_toolchain(self.path("image", "bin"), "aarch64-unknown-linux-gnu")
        env = ["PATH=%s:/usr/bin:/bin" % bin_dir, "RUSTUP_HOME=/usr/local/rustup"]
        self.images = {IMAGE: (IMAGE, env), NAMED: (NAMED_ID, env, [NAMED]),
                       RENAMED: (NAMED_ID, env, [NAMED])}
        self.docker_dir = self.path("docker")
        self.docker = build_fixtures.fake_docker(self.docker_dir, self.images)

    def linux_plan(self, run_id, pinned=None, image=None):
        """A Linux plan whose build is frozen to the recipe's `pinned` image or `image`."""
        def linux(recipe):
            recipe.update(os="linux", target="x86_64-unknown-linux-gnu", linker="system_cc")
            if pinned is not None:
                recipe["container_image"] = pinned
        return plan_for(self.repo, self.base, self.head, self.authority, run_id=run_id,
                        recipe_edit=linux, build_image=image)

    def builder(self, plan, **options):
        options.setdefault("free_bytes", PLENTY)
        options.setdefault("docker", self.docker)
        return builds.Builder(self.repo.path, plan, None, registry(), **options)

    def test_local_bounded_build_uses_private_cache_and_enforces_container_and_disk_limits(self):
        builder = self.builder(self.linux_plan("local_bounded", image=IMAGE), bounded_container=True)
        doc = builder.build()
        self.assertEqual(doc["cleanup"]["status"], "removed")
        self.assertTrue(builder.layout.private_cargo_home)
        calls = build_fixtures.docker_calls(self.docker_dir)
        self.assertTrue(any("{{json .Config.Volumes}}" in call for call in calls))
        steps = [call for call in calls if call[0] == "run" and "--name" in call
                 and call[call.index("--name") + 1].startswith("af-perf-build-")]
        self.assertTrue(steps)
        for call in steps:
            for flag, value in (("--memory", "8g"), ("--memory-swap", "8g"),
                                ("--pids-limit", "512"), ("--ipc", "none"),
                                ("--log-driver", "none"),
                                ("--ulimit", "fsize=8589934592:8589934592")):
                self.assertEqual(call[call.index(flag) + 1], value)
        # Private Cargo/home/tmp data counts alongside targets, even outside worktrees.
        os.mkdir(builder.layout.home)
        with open(os.path.join(builder.layout.home, "candidate-output"), "wb") as stream:
            stream.write(b"x" * 128)
        builder.budget.worktree_limit = 1
        self.assertIsNotNone(builder.budget.exceeded())
        with self.assertRaisesRegex(ContractError, "private Cargo"):
            self.builder(self.linux_plan("shared_cache", image=IMAGE), bounded_container=True,
                         cargo_home=self.path("shared"))

    def test_every_compile_runs_without_network_in_its_own_container_of_the_pinned_image(self):
        builder = self.builder(self.linux_plan("contained", image=IMAGE))
        doc = builder.build()
        isolation = doc["isolation"]
        self.assertEqual((isolation["kind"], isolation["compile_network"],
                          isolation["filesystem"], isolation["dependency_fetch"]),
                         ("container", "none", "run_mounts_only", "container_network"))
        self.assertEqual(isolation["image"], {"reference": IMAGE, "id": IMAGE,
                                              "platform": "linux/arm64"})
        self.assertEqual(doc["toolchain"]["host"], "aarch64-unknown-linux-gnu")
        for side in ("baseline", "candidate"):
            self.assertEqual(doc["sides"][side]["status"], "succeeded", doc["sides"][side])
        runs = [call for call in build_fixtures.docker_calls(self.docker_dir)
                if call[0] == "run"]
        steps = [call for call in runs if call[call.index("--name") + 1].startswith(
            "af-perf-build-")]
        # Three fetches, the supervisor, two products and three probe compiles: the
        # baseline lacks the first adapter's API, so its second adapter compiles.
        self.assertEqual(len(steps), 9, [call[-1] for call in steps])
        layout = builder.layout
        for call in steps:
            network = call[call.index("--network") + 1]
            fetch = "fetch" in call
            self.assertEqual(network, "bridge" if fetch else "none", call)
            for flag in ("--read-only", "--pull"):
                self.assertIn(flag, call)
            self.assertEqual(call[call.index("--pull") + 1], "never")
            self.assertEqual(call[call.index("--cap-drop") + 1], "ALL")
            self.assertEqual(call[call.index("--user") + 1],
                             "%d:%d" % (os.getuid(), os.getgid()))
            mounts = [call[i + 1] for i, flag in enumerate(call) if flag == "--mount"]
            sources = [mount.split(",")[1][len("src="):] for mount in mounts]
            for source in sources:
                # Nothing but the run's own directories, never the checkout or a home.
                self.assertTrue(source.startswith(layout.run + os.sep), source)
            self.assertIn("type=bind,src=%s,dst=%s,readonly" % (layout.git, layout.git),
                          mounts)
            # Exactly the allowlisted variables, through env -i: nothing of the image's own.
            self.assertEqual(call[call.index("--entrypoint") + 1:call.index("--entrypoint") + 4],
                             ["/usr/bin/env", IMAGE, "-i"])
            start = call.index("--entrypoint") + 4
            end = start
            while "=" in call[end] and not call[end].startswith("/"):
                end += 1
            env = dict(item.split("=", 1) for item in call[start:end])
            self.assertEqual(env["HOME"], layout.home)
            self.assertNotIn("RUSTUP_HOME", env)
            self.assertEqual(sorted(set(env) - set(build_fixtures.ALLOWED)), [])
        probe = [call for call in steps if "--example" in call
                 and call[call.index("--example") + 1] == "perf-probe"]
        self.assertEqual(len(probe), 2)
        for call in probe:
            overlays = [call[i + 1] for i, flag in enumerate(call) if flag == "--mount"
                        and call[i + 1].endswith(",readonly")
                        and "/overlays/" in call[i + 1]]
            # The authority's closure is bound read-only over the overlay in the worktree.
            self.assertEqual(len(overlays), 2, call)
            for mount in overlays:
                self.assertIn("/crates/review-pipeline/examples/perf-probe", mount)
        # Every container was removed and its removal confirmed.
        names = [call[call.index("--name") + 1] for call in runs]
        removed = [call[-1] for call in build_fixtures.docker_calls(self.docker_dir)
                   if call[0] == "rm"]
        self.assertEqual(sorted(names), sorted(removed))
        _, paths = builds.verify_artifacts(layout.build)
        self.assertIn(b"FIXTURE-AF commit=" + self.head[:12].encode(),
                      read(paths["candidate/af"]))

    def test_a_build_runs_exactly_the_frozen_build_and_never_chooses_another(self):
        # Planning a Linux recipe without a pin refuses unless the build is frozen there.
        with self.assertRaises(ContractError) as caught:
            self.linux_plan("no_image")
        self.assertIn("--trusted-host-build", str(caught.exception))
        frozen = self.linux_plan("frozen", image=IMAGE)
        run = os.path.join(self.repo.path, "target", "performance", "frozen")
        # A late --build-image may only restate the frozen image; nothing else is chosen.
        for image in (OTHER_IMAGE, NAMED, "rust:1.88"):
            with self.subTest(image=image):
                with self.assertRaises(builds.Unavailable) as caught:
                    self.builder(frozen, build_image=image).build()
                self.assertEqual(caught.exception.reason, "build_image_mismatch")
                self.assertFalse(os.path.exists(run))
        absent = self.linux_plan("absent", image="sha256:" + "c" * 64)
        with self.assertRaises(builds.Unavailable) as caught:
            self.builder(absent).build()
        self.assertEqual(caught.exception.reason, "build_image_unavailable")
        # A manifest digest names its image only under exactly its repository name, and the
        # build records that reference with the image's own, different ID.
        renamed = self.linux_plan("renamed", image=RENAMED)
        with self.assertRaises(builds.Unavailable) as caught:
            self.builder(renamed).build()
        self.assertEqual(caught.exception.reason, "build_image_mismatch")
        named = self.builder(self.linux_plan("named", image=NAMED), build_image=NAMED).build()
        self.assertEqual(named["isolation"]["image"], {"reference": NAMED, "id": NAMED_ID,
                                                       "platform": "linux/arm64"})
        # The recipe's own pin is the frozen image.
        pinned = self.linux_plan("pinned", pinned=IMAGE)
        doc = self.builder(pinned).build()
        self.assertEqual(doc["isolation"]["image"]["id"], IMAGE)
        self.assertEqual(doc["recipe_digest"], pinned["profile"]["recipe_digest"])
        macos = plan_for(self.repo, self.base, self.head, self.authority, run_id="macos")
        with self.assertRaises(builds.Unavailable) as caught:
            self.builder(macos, build_image=IMAGE).build()
        self.assertEqual(caught.exception.reason, "build_image_mismatch")

    def test_an_unconfirmed_container_removal_refuses_the_build(self):
        docker = build_fixtures.fake_docker(self.path("lingering-docker"), self.images,
                                            lingering="af-perf-build-")
        builder = self.builder(self.linux_plan("lingering", image=IMAGE), docker=docker)
        with self.assertRaises(builds.Unavailable) as caught:
            builder.build()
        self.assertEqual(caught.exception.reason, "build_container_unconfirmed")
        self.assertFalse(os.path.exists(os.path.join(builder.layout.run, "worktrees")))

    def test_a_cargo_home_holding_credentials_or_configuration_is_refused(self):
        for name in ("credentials.toml", "config.toml"):
            with self.subTest(name=name):
                home = self.path("cargo-home-" + name)
                os.makedirs(home)
                with open(os.path.join(home, name), "w") as stream:
                    stream.write("[registry]\ntoken = \"secret\"\n")
                builder = self.builder(self.linux_plan("home_" + name.split(".")[0],
                                                       image=IMAGE), cargo_home=home)
                with self.assertRaises(builds.Unavailable) as caught:
                    builder.build()
                self.assertEqual(caught.exception.reason, "cargo_home_credentials")
                self.assertFalse(os.path.exists(builder.layout.run))

    def test_a_build_script_cannot_publish_a_rewritten_closure_from_a_container(self):
        # The real container binds the closure read-only, so a rewrite fails there with EROFS;
        # the stand-in cannot, and the integrity check still refuses the build.
        repo, base, head, authority = self.repo, self.base, self.head, self.authority
        mutated = repo.commit("mutating", {"crates/review-pipeline/src/lib.rs":
                                           "pub fn answer_v2() -> u32 { 43 } // MUTATE_OVERLAY\n"})
        def linux(recipe):
            recipe.update(os="linux", target="x86_64-unknown-linux-gnu", linker="system_cc")
        plan = plan_for(repo, base, mutated, authority, run_id="mutating", recipe_edit=linux,
                        build_image=IMAGE)
        builder = self.builder(plan)
        with self.assertRaises(builds.OverlayIntegrity):
            builder.build()
        self.assertFalse(os.path.exists(os.path.join(builder.layout.build, builds.BUILDS_FILE)))


class Steps(TempDir):
    """Each build step has a deadline, a worktree budget and a capped log."""

    def setUp(self):
        TempDir.setUp(self)
        self.layout = builds.Layout(self.tmp, "step_run")
        self.layout.create()
        self.limits = {"worktree_bytes": 1 << 40, "build_artifact_bytes": 1 << 30,
                       "free_disk_floor_bytes": 10 << 30, "build_minutes": 1}

    def run_step(self, script, log_bytes=1 << 20, **limits):
        budget = builds.Budget(self.layout, dict(self.limits, **limits), log_bytes, PLENTY)
        return builds.Steps(self.layout, budget), budget

    def test_the_log_is_capped_and_truncation_counted(self):
        steps, _ = self.run_step(None, log_bytes=1000)
        step = steps.run("flood", ["/bin/sh", "-c", "head -c 100000 /dev/zero"], self.tmp,
                         {"PATH": "/usr/bin:/bin"})
        self.assertEqual(step["status"], "succeeded")
        self.assertEqual(step["log"]["bytes"], 1000)
        self.assertEqual(step["log"]["truncated_bytes"], 99000)

    def test_a_deadline_and_a_budget_stop_the_step_and_its_session(self):
        original = builds.POLL_SECONDS
        builds.POLL_SECONDS = 0.1
        self.addCleanup(setattr, builds, "POLL_SECONDS", original)
        steps, budget = self.run_step(None)
        budget.deadline_seconds = 1
        step = steps.run("slow", ["/bin/sh", "-c", "sleep 60 & sleep 60"], self.tmp,
                         {"PATH": "/usr/bin:/bin"})
        self.assertEqual(step["status"], "timeout")
        self.assertLess(step["elapsed_ms"], 30000)
        with open(os.path.join(self.layout.worktrees, "big"), "wb") as stream:
            stream.write(b"x" * 4096)
        steps, _ = self.run_step(None, worktree_bytes=1024)
        step = steps.run("fat", ["/bin/sh", "-c", "sleep 60"], self.tmp,
                         {"PATH": "/usr/bin:/bin"})
        self.assertEqual(step["status"], "budget_exceeded")

    def test_the_run_s_total_deadline_ends_a_step_and_starts_no_other(self):
        original = builds.POLL_SECONDS
        builds.POLL_SECONDS = 0.1
        self.addCleanup(setattr, builds, "POLL_SECONDS", original)
        steps, budget = self.run_step(None)
        # The step's own deadline is a minute; the run's total deadline, one second away,
        # dominates, and the step's whole session is ended (ADR-0170).
        budget.run_deadline = time.monotonic() + 1
        marker = os.path.join(self.tmp, "escaped")
        step = steps.run("slow", ["/bin/sh", "-c", "(sleep 3; touch %s) & sleep 60" % marker],
                         self.tmp, {"PATH": "/usr/bin:/bin"})
        self.assertEqual(step["status"], "timeout")
        self.assertLess(step["elapsed_ms"], 30000)
        self.assertEqual(step["deadline_seconds"], 60)
        time.sleep(3.5)
        self.assertFalse(os.path.exists(marker), "the step's session outlived the deadline")
        # Once it has passed, no further step is started.
        step = steps.run("after", ["/bin/sh", "-c", "touch %s" % marker], self.tmp,
                         {"PATH": "/usr/bin:/bin"})
        self.assertEqual((step["status"], step["exit_code"]), ("timeout", None))
        self.assertFalse(os.path.exists(marker))


class Registry(TempDir):
    def test_the_committed_adapters_serve_every_native_contract_with_their_closure(self):
        from perflab import suite as suite_module
        doc = adapters.load(os.path.join(ROOT, adapters.ADAPTERS_PATH))
        adapters.check_registry(doc, suite_module.load(os.path.join(
            ROOT, "fixtures/performance/suite.toml")).doc)
        adapters.check_closure(doc, ROOT)

    def test_a_closure_without_its_helper_modules_is_refused(self):
        doc = adapters.load(os.path.join(ROOT, adapters.ADAPTERS_PATH))
        entry = doc["adapters"]["native_probe_public_api_v1"]
        entry["closure"] = ["crates/review-pipeline/examples/perf-probe.rs"]
        with self.assertRaises(ContractError) as caught:
            adapters.check_closure(doc, ROOT)
        self.assertIn("perf-probe/cases.rs", str(caught.exception))


def real_toolchain():
    try:
        return builds.Toolchain.discover("1.88.0"), None
    except builds.Unavailable as error:
        return None, str(error)


REAL, MISSING = real_toolchain()
REAL_PROFILES = {"aarch64-apple-darwin": ("nightly", "macos_arm64_local"),
                 "x86_64-unknown-linux-gnu": ("quick", "linux_x86_64_gnu_hosted")}

AF_BUILD_RS = '''use std::process::Command;
fn main() {
    let commit = Command::new("git").args(["rev-parse", "--short=12", "HEAD"]).output().ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=AF_GIT_COMMIT={commit}");
}
'''


@unittest.skipUnless(REAL is not None and REAL.host in REAL_PROFILES,
                     "the pinned Rust 1.88.0 toolchain for a declared recipe host is not "
                     "installed: %s" % (MISSING or platform.machine()))
class RealToolchain(TempDir):
    """The pinned toolchain builds a minimal real workspace from detached worktrees."""

    def test_real_builds_embed_their_commit_through_the_worktree_indirection(self):
        repo = Repo(self.path("checkout"))
        files = {
            ".gitignore": "/target\n",
            "Cargo.toml": ('[workspace]\nresolver = "2"\nmembers = ["crates/af", '
                           '"crates/review-pipeline", "crates/review-process"]\n'),
            "crates/af/Cargo.toml": ('[package]\nname = "af"\nversion = "0.0.0"\n'
                                     'edition = "2021"\npublish = false\n'),
            "crates/af/build.rs": AF_BUILD_RS,
            "crates/af/src/main.rs": 'fn main() { println!("{}", env!("AF_GIT_COMMIT")); }\n',
            "crates/review-pipeline/Cargo.toml": ('[package]\nname = "review-pipeline"\n'
                                                  'version = "0.0.0"\nedition = "2021"\n'
                                                  'publish = false\n'),
            "crates/review-pipeline/src/lib.rs": "pub fn answer() -> u32 { 42 }\n",
            "crates/review-pipeline/examples/perf-probe.rs":
                'fn main() { println!("own {}", review_pipeline::answer()); }\n',
            "crates/review-process/Cargo.toml": ('[package]\nname = "review-process"\n'
                                                 'version = "0.0.0"\nedition = "2021"\n'
                                                 'publish = false\n'),
            "crates/review-process/src/lib.rs": "pub fn supervise() {}\n",
            "crates/review-process/examples/perf-supervisor.rs": "fn main() {}\n",
        }
        for relative, data in files.items():
            repo.write(relative, data)
        env = {"PATH": REAL.bin_dir + ":/usr/bin:/bin", "HOME": self.path("home"),
               "CARGO_HOME": self.path("cargo-home"), "LC_ALL": "C"}
        subprocess.run([REAL.cargo, "generate-lockfile", "--offline"], cwd=repo.path, env=env,
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        base = repo.commit("base")
        head = repo.commit("head", {"crates/review-pipeline/src/lib.rs":
                                    "pub fn answer() -> u32 { 42 }\n"
                                    "pub fn answer_v2() -> u32 { 43 }\n"})
        authority = repo.commit("authority", {
            "crates/review-pipeline/examples/perf-probe.rs":
                '#[path = "perf-probe/helper.rs"]\nmod helper;\n'
                'fn main() { println!("authority {}", helper::value()); '
                'std::process::exit(3); }\n',
            "crates/review-pipeline/examples/perf-probe/helper.rs":
                "pub fn value() -> u32 { review_pipeline::answer_v2() }\n",
            "crates/review-pipeline/examples/perf-probe-v1.rs":
                'fn main() { println!("bridge {}", review_pipeline::answer()); }\n'})
        tier, environment = REAL_PROFILES[REAL.host]
        # This fixture intentionally compiles on the test host. Its purpose is commit and
        # adapter provenance, and it makes no container-isolation claim on Linux either.
        plan = plan_for(repo, base, head, authority, run_id="real_build_run", tier=tier,
                        environment=environment,
                        trusted_host_build=environment.startswith("linux_"))
        builder = builds.Builder(repo.path, plan, REAL, registry(), free_bytes=PLENTY)
        # Admit this fixture's real C linker in the sanitized build path.
        cc = shutil.which("cc")
        self.assertIsNotNone(cc, "the real toolchain fixture needs a C linker")
        with mock.patch.object(builds, "SYSTEM_PATH",
                               os.path.dirname(cc) + ":" + builds.SYSTEM_PATH):
            doc = builder.build()
        base_side, cand = doc["sides"]["baseline"], doc["sides"]["candidate"]
        self.assertEqual(doc["supervisor"]["status"], "succeeded",
                         read(os.path.join(builder.layout.logs, "supervisor.log")))
        for side, sha in ((base_side, base), (cand, head)):
            self.assertEqual(side["status"], "succeeded", side)
            self.assertEqual(side["embedded_commit"]["expected"], sha[:12])
            self.assertTrue(side["embedded_commit"]["verified"])
        self.assertEqual(base_side["chosen_adapter"], "fixture_probe_v1")
        self.assertEqual(cand["chosen_adapter"], "fixture_probe_v2")
        self.assertEqual(base_side["adapter_outcomes"][0]["outcome"], "failed")
        self.assertEqual(doc["toolchain"]["rustc"], REAL.rustc_identity)
        _, paths = builds.verify_artifacts(builder.layout.build)
        # The binaries are the fixture's own; running them shows what they embedded and that
        # the candidate's probe is the authority's. That probe fails at runtime, and was chosen
        # all the same: selection compiles, it never runs anything.
        for name, expected, code in (("baseline/af", base[:12], 0),
                                     ("candidate/af", head[:12], 0),
                                     ("candidate/probe", "authority 43", 3),
                                     ("baseline/probe", "bridge 42", 0)):
            output = subprocess.run([paths[name]], stdout=subprocess.PIPE)
            self.assertEqual((output.stdout.decode().strip(), output.returncode),
                             (expected, code))
        self.assertEqual(doc["cleanup"]["status"], "removed")
        self.assertEqual(os.listdir(builder.layout.run), ["build"])


if __name__ == "__main__":
    unittest.main()
