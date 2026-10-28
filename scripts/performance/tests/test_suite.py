"""The reviewed registry: completeness, cross-references, source pins and the TOML reader."""
import hashlib
import json
import os
import re
import sys
import unittest

from perflab import limits, suite, tomlfile
from perflab.values import ContractError
from tests.support import ROOT, SUITE, SYNTHETIC_SUITE, read, replace_once

VENDOR = os.path.join(ROOT, "scripts/performance/perflab/_vendor")


def load_text(text):
    return suite.load_bytes(text.encode("utf-8"), "edited-suite.toml")


class Registry(unittest.TestCase):
    def setUp(self):
        self.text = read(SUITE).decode("utf-8")
        self.suite = suite.load(SUITE)

    def refuses(self, old, new, message):
        with self.assertRaises(ContractError) as caught:
            load_text(replace_once(self.text, old, new))
        self.assertIn(message, str(caught.exception))

    def test_initial_coverage_expands_to_the_design_cases(self):
        ids = dict((tier, [item["instance_id"] for item in self.suite.instances(tier)])
                   for tier in suite.TIERS)
        self.assertEqual((len(ids["quick"]), len(ids["nightly"]), len(ids["release"])),
                         (30, 33, 11))
        for registry in ("empty", "stores_20x10000"):
            for tier in suite.TIERS:
                self.assertIn("tui_open__registry_" + registry, ids[tier])
        for command in ("version", "help", "task_help", "review_run_help"):
            for tier in suite.TIERS:
                self.assertIn("cli_discovery__command_" + command, ids[tier])
        for command in ("task_show", "review_ledger", "review_report"):
            self.assertIn("cli_state_read__command_%s_state_cli_state_10000" % command,
                          ids["release"])
        self.assertIn("source_capture__files_100_tree_source_files_128b", ids["quick"])
        self.assertNotIn("source_capture__files_10000_tree_source_files_128b", ids["quick"])
        self.assertIn("source_capture__files_10000_tree_source_files_128b", ids["nightly"])
        for package in ("issue_255_medium", "issue_255_large"):
            for case in ("canonical_serialize", "canonical_validate", "cas_raw_get",
                         "cas_typed_get", "authority_plan_check", "authority_runtime_new"):
                self.assertIn("%s__package_%s" % (case, package), ids["quick"])
        advisory = [item["instance_id"] for item in self.suite.instances("nightly")
                    if item["criticality"] == "advisory"]
        self.assertEqual(advisory, ["persistence_fs__operation_cas_write_read",
                                    "persistence_fs__operation_state_append_read"])
        for tier in suite.TIERS:
            self.assertEqual(ids[tier], sorted(ids[tier]))

    def test_initial_sampling_policy(self):
        cases = self.suite.doc["cases"]
        cheap = ("cli_discovery", "canonical_serialize_small", "canonical_validate_small",
                 "cas_raw_get_small", "cas_typed_get_small")
        for case_id, case in cases.items():
            expected = 40 if case_id in cheap else 20
            if "quick" in case["tiers"]:
                self.assertEqual(case["pairs"]["quick"], expected, case_id)
            for tier in ("nightly", "release"):
                if tier in case["tiers"]:
                    self.assertEqual(case["pairs"][tier], 40, case_id)
            self.assertEqual(case["warmup_pairs"], 2)
            self.assertEqual(case["policy"], "wall_time_default")
        policy = self.suite.doc["policies"]["wall_time_default"]
        self.assertEqual((policy["relative_tolerance"], policy["absolute_tolerance"]),
                         ("0.05", "1"))

    def test_browser_budget_cannot_be_redirected_to_splash(self):
        self.refuses('field = "browser_ms"', 'field = "splash_ms"',
                     "only tui_open reports browser_ms")

    def test_browser_budget_covers_both_registry_instances(self):
        case = self.suite.doc["cases"]["tui_open"]
        budget = self.suite.doc["budgets"]["tui_browser_under_200ms"]
        self.assertEqual(case["parameters"]["registry"]["values"],
                         ["empty", "stores_20x10000"])
        self.assertEqual(case["budgets"], ["tui_browser_under_200ms"])
        self.assertEqual(case["timing"]["boundary"], "reported_interval")
        self.assertEqual(case["timing"]["field"], "browser_ms")
        self.assertEqual(case["sample_timeout_seconds"], 120)
        self.assertEqual((budget["limit"], budget["comparison"], budget["minimum_samples"]),
                         ("200", "every_sample_strictly_below", 20))
        self.assertEqual(budget["state"], "pending_calibration")
        self.assertEqual(budget["environments"],
                         ["linux_x86_64_musl_hosted", "macos_arm64_local"])

    def test_executable_states_are_exactly_the_validated_paths(self):
        # p3b (ADR-0169): every recipe, environment and case is implemented and executable
        # where a host provides it; a host that cannot refuses at run time, never through a
        # registry state, and every critical instance stays declared.
        doc = self.suite.doc
        for table in ("recipes", "environments"):
            for key, value in doc[table].items():
                self.assertEqual((value["state"], value["pending"]), ("supported", ""), key)
        self.assertEqual(doc["probe"]["state"], "supported")
        self.assertEqual(doc["health"]["oracle"]["state"], "available")
        for key, case in doc["cases"].items():
            self.assertEqual(case["execution"]["state"], "supported", key)
            self.assertEqual(case["oracle"]["state"], "available", key)
            if case["kind"] == "library":
                self.assertEqual(case["probe"]["repetitions"], 1, key)
                self.assertIn("setup_ms", case["optional_metrics"], key)
            else:
                self.assertNotIn("probe", case, key)
        spawning = sorted(key for key, case in doc["cases"].items()
                          if case["requires_linux_containment"])
        self.assertEqual(spawning, ["cli_state_read", "cli_task_run_check", "cli_task_start",
                                    "source_capture"])
        for key, fixture in doc["fixtures"].items():
            pending = fixture["kind"] in ("cli_state", "cli_task", "cli_registry")
            self.assertEqual(fixture["golden"], "pending_native_oracle" if pending
                             else "available", key)
        for key, budget in doc["budgets"].items():
            self.assertEqual(budget["state"], "pending_calibration", key)

    def test_probe_declarations_are_checked(self):
        self.refuses('\nprobe = { contract_id = "canonical_serialize_v1", repetitions = 1 }\n',
                     "\n", "names its probe contract")
        self.refuses('budgets = ["cli_discovery_under_50ms"]',
                     'budgets = ["cli_discovery_under_50ms"]\n'
                     'probe = { contract_id = "cli_v1", repetitions = 1 }',
                     "only library cases")
        self.refuses('probe = { contract_id = "canonical_serialize_v1", repetitions = 1 }',
                     'probe = { contract_id = "cas_raw_get_verify_v1", repetitions = 1 }',
                     "whose timing region or cache treatment differs")
        self.refuses('probe = { contract_id = "canonical_serialize_v1", repetitions = 1 }',
                     'probe = { contract_id = "canonical_serialize_v1", repetitions = 2 }',
                     "1..1")
        self.refuses('sentinel_contract = "sentinel_canonical_v1"',
                     'sentinel_contract = "canonical_validate_v1"', "its own contract")
        self.refuses('state = "supported"\npending = ""\ndescription = "Native public-API',
                     'state = "not_yet_executable"\npending = ""\ndescription = "Native public-API',
                     "say what the probe is pending")
        self.refuses('state = "supported"\npending = ""\ndescription = "Native public-API',
                     'state = "not_yet_executable"\npending = "p2"\n'
                     'description = "Native public-API', "needs a supported probe")
        self.refuses('golden = "available"', 'golden = "pending_native_oracle"',
                     "needs a supported probe and the workload's golden entries")
        self.refuses('serialized_envelope_bytes = 5605230\nsize_rule = "exact"\n'
                     'golden = "available"', 'serialized_envelope_bytes = 5605230\n'
                     'size_rule = "exact"\ngolden = "pending_native_oracle"',
                     "fixture issue_255_large has no golden entries")

    def test_recipes_bind_the_source_pinned_toolchain(self):
        toolchain = re.search(r'channel = "([^"]+)"',
                              read(os.path.join(ROOT, "rust-toolchain.toml")).decode()).group(1)
        release = read(os.path.join(ROOT, ".github/workflows/release.yml")).decode()
        pins = read(os.path.join(ROOT, "scripts/release-toolchain.txt")).decode()
        self.assertIn("--require-hashes --only-binary=:all: -r scripts/release-toolchain.txt",
                      release)
        zigbuild = re.search(r"^cargo-zigbuild==([0-9.]+)", pins, re.MULTILINE).group(1)
        zig = re.search(r"^ziglang==([0-9.]+)", pins, re.MULTILINE).group(1)
        for key, recipe in self.suite.doc["recipes"].items():
            self.assertEqual(recipe["toolchain"], toolchain, key)
            if recipe["builder"] == "cargo_zigbuild":
                self.assertEqual((recipe["cargo_zigbuild"], recipe["zig"]), (zigbuild, zig))
        shipping = self.suite.doc["recipes"]["linux_musl_shipping"]
        self.assertIn(shipping["target"], release)

    def test_product_constraints_match_the_production_source(self):
        lease = read(os.path.join(ROOT, "crates/review-pipeline/src/task/lease.rs")).decode()
        cas = read(os.path.join(ROOT, "crates/review-store/src/cas.rs")).decode()
        probe = read(os.path.join(ROOT, "crates/review-pipeline/examples/perf-probe/"
                                  "fixtures.rs")).decode()
        self.assertIn("pub const TASK_LEASE_MS: u64 = 60_000;", lease)
        self.assertIn("pub const TASK_LEASE_MS: u64 = 60_000;", probe)
        self.assertIn("const MAX_ARTIFACT_ENVELOPE_BYTES: u64 = 8 * 1024 * 1024;", cas)
        constraints = self.suite.doc["product_constraints"]
        self.assertEqual(constraints["task_lease_ms"], 60000)
        self.assertEqual(constraints["artifact_envelope_cap_bytes"], 8 * 1024 * 1024)
        large = self.suite.doc["fixtures"]["issue_255_large"]
        self.assertEqual((large["serialized_envelope_bytes"], large["size_rule"]),
                         (5605230, "exact"))

    def test_unknown_fields_floats_and_dates_fail_closed(self):
        self.refuses('suite_id = "af_performance"', 'suite_id = "af_performance"\nextra = 1',
                     "unknown field extra")
        self.refuses('relative_tolerance = "0.05"', "relative_tolerance = 0.05",
                     "TOML floats are not allowed")
        self.refuses('suite_id = "af_performance"', 'suite_id = "af_performance"\n'
                     "when = 2026-01-01", "dates and times are not allowed")
        # Version 4 admits the browser's reported interval; earlier shapes fail closed.
        for earlier in ("1", "2", "3"):
            self.refuses('schema = "af.perf-suite/4"', 'schema = "af.perf-suite/%s"' % earlier,
                         "expected 'af.perf-suite/4'")
        self.refuses('request = "af.perf-probe-request/2"', 'request = "af.perf-probe-request/1"',
                     "expected 'af.perf-probe-request/2'")

    def test_references_and_policies_are_checked(self):
        self.refuses('policy = "wall_time_default"\npairs = { quick = 40, nightly = 40, '
                     'release = 40 }', 'policy = "missing"\npairs = { quick = 40, nightly = 40, '
                     'release = 40 }', "unknown policy")
        self.refuses('relative_tolerance = "0.05"', 'relative_tolerance = "0.1"',
                     "must be calibrated")
        self.refuses('absolute_tolerance = "1"', 'absolute_tolerance = "0"',
                     "strictly positive")
        self.refuses('budgets = ["cli_discovery_under_50ms"]', 'budgets = ["nope"]',
                     "unknown budget")
        self.refuses('values = ["issue_255_medium", "issue_255_large"]',
                     'values = ["issue_255_medium", "issue_255_huge"]', "unknown fixture")
        self.refuses("tier_parameters = { quick = { files = [100] } }",
                     "tier_parameters = { quick = { files = [101] } }", "outside the domain")
        self.refuses("tier_parameters = { quick = { files = [100] } }",
                     'tier_parameters = { quick = { files = ["100"] } }', "expected an integer")

    def test_sampling_and_resource_bounds(self):
        self.refuses("pairs = { quick = 40, nightly = 40, release = 40 }",
                     "pairs = { quick = 41, nightly = 40, release = 40 }", "even count")
        self.refuses("pairs = { quick = 40, nightly = 40, release = 40 }",
                     "pairs = { quick = 202, nightly = 40, release = 40 }", "12..200")
        self.refuses("pairs = { quick = 40, nightly = 40, release = 40 }",
                     "pairs = { quick = 10, nightly = 40, release = 40 }", "12..200")
        self.refuses("pairs = { quick = 40, nightly = 40, release = 40 }",
                     "pairs = { quick = 40, nightly = 40 }", "exactly the case's tiers")
        self.refuses("warmup_pairs = 2", "warmup_pairs = 12", "0..10")
        self.refuses("sample_timeout_seconds = 30", "sample_timeout_seconds = 121", "1..120")
        self.refuses("free_disk_floor_bytes = 10737418240", "free_disk_floor_bytes = 1073741824",
                     "free_disk_floor_bytes")
        self.refuses("shard_sampling_minutes = 25", "shard_sampling_minutes = 31",
                     "hard maximum")
        self.refuses("serialized_envelope_bytes = 5605230", "serialized_envelope_bytes = 8388609",
                     "serialized_envelope_bytes")

    def test_budget_needs_enough_pairs(self):
        self.refuses("minimum_samples = 20\nstate = \"pending_calibration\"\n"
                     "tiers = [\"nightly\", \"release\"]\n"
                     "environments = [\"linux_x86_64_musl_hosted\", \"macos_arm64_local\"]",
                     "minimum_samples = 41\nstate = \"pending_calibration\"\n"
                     "tiers = [\"nightly\", \"release\"]\n"
                     "environments = [\"linux_x86_64_musl_hosted\", \"macos_arm64_local\"]",
                     "fewer than the budget")

    def test_capabilities_states_are_honest(self):
        self.refuses('target = "aarch64-apple-darwin"\n',
                     'target = "aarch64-apple-darwin"\ncontainer_image = "sha256:%s"\n'
                     % ("a" * 64), "only a Linux recipe builds in a container image")
        self.refuses('linker = "apple_ld"\nfeatures = []\nrustflags = []\ninstrumented = false\n'
                     'debug_symbols = false\nframe_pointers = false\nstate = "supported"\n'
                     'pending = ""',
                     'linker = "apple_ld"\nfeatures = []\nrustflags = []\ninstrumented = false\n'
                     'debug_symbols = false\nframe_pointers = false\nstate = "supported"\n'
                     'pending = "later"', "a supported recipe has no pending note")
        case = self.text.split("[cases.cli_state_read]", 1)[1]
        oracle = case[case.index("oracle = {"):]
        oracle = oracle[:oracle.index("\n")]
        self.refuses(oracle, oracle.replace('state = "available"',
                                            'state = "pending_cli_fixture"'),
                     "available independent oracle")

    def test_the_default_profile_must_contain_every_case(self):
        text = replace_once(self.text, '[tiers.quick]\ndescription = "Pull requests and main '
                            'pushes."\nprofiles = [\n  { environment = "linux_x86_64_gnu_hosted", '
                            'recipe = "linux_gnu_native_release" },',
                            '[tiers.quick]\ndescription = "Pull requests and main pushes."\n'
                            'profiles = [\n  { environment = "macos_arm64_local", '
                            'recipe = "macos_arm64_native_release" },')
        with self.assertRaises(ContractError) as caught:
            load_text(text)
        self.assertIn("cannot run cli_state_read", str(caught.exception))

    def test_instance_ids_obey_the_id_rule(self):
        renamed = replace_once(self.text, 'parameters = ["log"]',
                               'parameters = ["log_with_a_much_longer_name"]')
        with self.assertRaises(ContractError) as caught:
            load_text(replace_once(
                renamed, "parameters = { log = { kind = \"fixture\", values = "
                "[\"events_10000\"] }", "parameters = { log_with_a_much_longer_name = { "
                "kind = \"fixture\", values = [\"events_10000\"] }"))
        self.assertIn("exceeds the ID rule", str(caught.exception))

    def test_a_seed_is_written_over_parameters_of_its_case_by_a_writer_of_its_own(self):
        # ADR-0170: the seed's parameters are the case's, sorted, and its writer is no timed
        # case and no case's probe contract.
        self.refuses('parameters = ["log"]', 'parameters = ["projection_log"]',
                     "sorted parameters of the case")
        self.refuses('parameters = ["files", "tree"]', 'parameters = ["tree", "files"]',
                     "sorted parameters of the case")
        self.refuses('case_id = "state_projection_seed", contract_id = '
                     '"state_projection_seed_v1"', 'case_id = "state_projection", contract_id = '
                     '"state_projection_seed_v1"', "a seed writer is not a timed case")
        self.refuses('contract_id = "state_projection_seed_v1"',
                     'contract_id = "state_projection_v2"', "a writer contract is not a case's")
        seeded = [item for item in self.suite.instances("nightly")
                  if suite.seed_of(self.suite.doc, item) is not None]
        self.assertEqual(sorted(set(suite.seed_of(self.suite.doc, item)["seed_id"]
                                    for item in seeded)), [
            "authority_seed__package_issue_255_large",
            "authority_seed__package_issue_255_medium",
            "source_capture_seed__files_10000_tree_source_files_128b",
            "source_capture_seed__files_100_tree_source_files_128b",
            "state_projection_seed__log_events_10000"])
        self.assertEqual(len(seeded), 8)

    def test_a_recipe_pins_its_build_image_by_a_canonical_digest(self):
        pin = 'target = "x86_64-unknown-linux-gnu"\n'
        for image in ("sha256:" + "a" * 64, "ghcr.io/example/af-build@sha256:" + "b" * 64):
            load_text(replace_once(self.text, pin, pin + 'container_image = "%s"\n' % image))
        for image in ("ghcr.io/example/af-build:1.0", "Example/af@sha256:" + "b" * 64,
                      "example/af:1.0@sha256:" + "b" * 64):
            self.refuses(pin, pin + 'container_image = "%s"\n' % image, "does not match")

    def test_limits_are_the_hard_maxima_of_the_design(self):
        self.assertEqual((limits.PAIRS_MIN, limits.PAIRS_MAX, limits.WARMUP_PAIRS_MAX,
                          limits.INSTANCES_MAX, limits.SHARDS_MAX, limits.CONCURRENT_JOBS_MAX),
                         (12, 200, 10, 64, 16, 8))
        self.assertEqual(limits.FREE_DISK_FLOOR_BYTES, 10 * 1024 ** 3)
        self.assertEqual((limits.DIAGNOSTIC_HZ_MAX, limits.DIAGNOSTIC_SECONDS_MAX,
                          limits.DIAGNOSTIC_CASES_MAX), (1999, 30, 8))

    def test_synthetic_registry_is_labeled(self):
        loaded = suite.load(SYNTHETIC_SUITE)
        self.assertEqual(loaded.doc["suite_id"], "af_performance_synthetic")
        self.assertIn("SYNTHETIC", read(SYNTHETIC_SUITE).decode().splitlines()[0])
        for case in loaded.doc["cases"].values():
            self.assertTrue(case["description"].startswith("SYNTHETIC"))


class Vendored(unittest.TestCase):
    def test_tomli_is_the_unmodified_licensed_release(self):
        record = json.loads(read(os.path.join(VENDOR, "VENDOR.json")).decode())
        (package,) = record["packages"]
        self.assertEqual((package["name"], package["version"], package["modified"]),
                         ("tomli", "2.2.1", False))
        listed = set()
        for entry in package["files"]:
            data = read(os.path.join(VENDOR, entry["path"]))
            self.assertEqual(len(data), entry["size"], entry["path"])
            self.assertEqual("sha256:" + hashlib.sha256(data).hexdigest(), entry["sha256"],
                             entry["path"])
            listed.add(entry["path"])
        present = set()
        for directory, names, files in os.walk(os.path.join(VENDOR, "tomli")):
            names[:] = [name for name in names if name != "__pycache__"]
            for name in files:
                present.add(os.path.relpath(os.path.join(directory, name), VENDOR))
        self.assertEqual(present, listed)
        self.assertIn("tomli/LICENSE", listed)
        self.assertIn("MIT License", read(os.path.join(VENDOR, "tomli/LICENSE")).decode())
        from perflab._vendor import tomli
        self.assertEqual(tomli.__version__, "2.2.1")


# CI sets AF_PERF_REQUIRE_TOMLLIB on Python 3.11+ so this parity check can never be skipped.
@unittest.skipIf(sys.version_info < (3, 11) and not os.environ.get("AF_PERF_REQUIRE_TOMLLIB"),
                 "tomllib exists from Python 3.11")
class TomllibParity(unittest.TestCase):
    def test_vendored_tomli_and_tomllib_agree_on_every_registry(self):
        import tomllib
        for path in (SUITE, SYNTHETIC_SUITE):
            raw = read(path)
            self.assertEqual(tomlfile.loads(raw, path), tomlfile.loads(raw, path, tomllib))
            self.assertEqual(suite.load_bytes(raw, path).policy_digest,
                             suite.load_bytes(raw, path, tomllib).policy_digest)

    def test_both_parsers_refuse_floats_the_same_way(self):
        import tomllib
        for parser in (None, tomllib):
            with self.assertRaises(ContractError):
                tomlfile.loads(b"a = 1.5\n", "float.toml", parser)


if __name__ == "__main__":
    unittest.main()
