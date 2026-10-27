"""Frozen shard execution and aggregation (package p3b, ADR-0168).

Real Git authority fixtures, real verified build artifacts and real shard bundles judged by the
real comparator. The executed programs are stand-ins, never product code: a Python `af` that
answers the discovery commands, a Python probe that writes the golden entry's facts, and the
framed fake supervisor of `tests.support`, whose `overrides.json` replays outcomes the real
supervisor can report (a timeout, a leaked descendant, an uncertain cleanup, no reply, an
artifact changed mid-shard). No number here describes the AF product.
"""
import json
import os
import shutil
import subprocess
import sys
import time
from unittest import mock

from perflab import builds, bundle, clicases, compare, contracts, hostlock, oracle, planning
from perflab import runner
from perflab import schedule, supervise
from perflab import suite as suite_module
from perflab.values import ContractError, sha256_bytes
from tests.support import CLI, ROOT, Repo, SUITE, TempDir, fake_supervisor, observe_json
from tests.support import read, writable_copy

VERSION = "0.12.0-rc.3"
LOCK = ('version = 4\n\n[[package]]\nname = "af"\nversion = "%s"\n\n[[package]]\n'
        'name = "review-core"\nversion = "%s"\n' % (VERSION, VERSION))
DISCOVERY = "cli_discovery__command_version"
SMALL = "canonical_serialize_small__package_small_envelope"
TASK_SHOW = "cli_state_read__command_task_show_state_cli_state_10000"
COLD = "state_projection__log_events_10000_projection_application_cold"
WARM = "state_projection__log_events_10000_projection_prewarmed"
SOURCE_100 = "source_capture__files_100_tree_source_files_128b"
SOURCE_10000 = "source_capture__files_10000_tree_source_files_128b"
LEDGER = "cli_state_read__command_review_ledger_state_cli_state_10000"

FAKE_AF = r'''
import json, os, sys
SUMMARY = {"open": 0, "pending_verification": 0, "fixed": 0, "rejected": 0, "wontfix": 0,
           "contested": 0}
VERSION = %(version)r
SIDE = %(side)r
CONTROL = %(control)r
args = sys.argv[1:]
if os.environ.get("AF_SELF_OFFLINE") != "1":
    sys.stderr.write("CLI fixtures and samples must not start a release update check\n")
    sys.exit(10)


def state():
    return args[args.index("--state") + 1]


def read_private_copy(path):
    # The copy is this sample's own: a mark an earlier sample left would be shared state.
    with open(path) as stream:
        data = stream.read()
    if "read by a sample" in data:
        sys.stderr.write("this seed copy was already read by another sample\n")
        sys.exit(9)
    if os.path.exists(os.path.join(CONTROL, "tamper-seed-" + SIDE)):
        # Trusted local mode runs as the shard's user: the sealed seed in the private parent
        # of this sample's scratch is reachable, and changing it must stop the shard.
        for current, dirs, names in os.walk(os.path.dirname(os.getcwd())):
            if os.path.basename(current) == SIDE + "-task_state":
                target = os.path.join(current, "probe-work", "events.sqlite")
                os.chmod(target, 0o644)
                with open(target, "a") as stream:
                    stream.write("changed by a sample\n")
    with open(path, "a") as stream:
        stream.write("read by a sample\n")
    return data


if args == ["--version"]:
    sys.stdout.write("af %%s\n" %% VERSION)
elif args == ["--help"]:
    sys.stdout.write("Usage: af [OPTIONS] [COMMAND]\n\nCommands:\n  review  x\n  task    x\n\n"
                     "Options:\n  -V, --version\n")
elif args == ["task", "--help"]:
    sys.stdout.write("Usage: af task <COMMAND>\n\nCommands:\n  start  x\n  run    x\n"
                     "  show   x\n")
elif args == ["review", "run", "--help"]:
    sys.stdout.write("Usage: af review run [OPTIONS]\n  --campaign <NAME>\n"
                     "  --policy-rev <REV>\n")
elif args[:2] == ["review", "run"]:
    os.makedirs(state())
    with open(os.path.join(state(), "events.sqlite"), "w") as stream:
        stream.write("campaign reviewed by the %%s side\n" %% SIDE)
    sys.stdout.write('{"schema":"af/review-outcome@3"}\n')
elif args[:2] == ["task", "show"]:
    read_private_copy(os.path.join(state(), "events.sqlite"))
    kinds = ["opened", "plan_proposed", "admitted"] + ["lease_renewed"] * 9997
    sys.stdout.write('{"task_id":"perf-probe-task","history":[' + ",".join(
        '{"sequence":%%d,"transition":{"change":{"kind":"%%s"}}}' %% (index, kind)
        for index, kind in enumerate(kinds)) + "]}\n")
elif args[:2] == ["review", "ledger"]:
    read_private_copy(os.path.join(state(), "events.sqlite"))
    for index in range(1, 101):
        sys.stdout.write("sha256:%%064x\tminor\topen\tin\tminor\tsrc/fixture.txt:%%d\t"
                         "Fixture finding %%d\n" %% (index, index, index))
elif args[:2] == ["review", "report"]:
    read_private_copy(os.path.join(state(), "events.sqlite"))
    findings = [{"key": "sha256:%%064x" %% index, "title": "Fixture finding %%d" %% index,
                 "status": "open", "severity": "minor", "file": "src/fixture.txt",
                 "line": index} for index in range(1, 101)]
    sys.stdout.write(json.dumps({"schema": "af/review-report@4", "campaign": "perf-campaign",
                                 "runs_recorded": 1, "ledger_round": 1,
                                 "findings_summary": dict(SUMMARY, open=100),
                                 "findings": findings}) + "\n")
else:
    sys.exit(2)
'''

FAKE_PROBE = r'''
import json, os, sys
ENTRIES = %(entries)r
SLOW_AFTER = %(slow_after)r
request = json.loads(sys.stdin.read())
SIDE = %(side)r
CONTROL = %(control)r
if request["case_id"] in ("cli_state_fixture", "cli_campaign_fixture"):
    # A seed writer: the side's own state, and the message setup.sh checks.
    writes = os.path.join(CONTROL, "seed-writes-" + SIDE)
    with open(writes, "a") as stream:
        stream.write(request["case_id"] + "\n")
    if os.path.exists(os.path.join(CONTROL, "fail-seed-" + SIDE)):
        sys.stderr.write("the stand-in writer fails\n")
        sys.exit(1)
    os.makedirs(os.path.join(request["scratch"], "probe-work"))
    if request["case_id"] == "cli_state_fixture":
        with open(os.path.join(request["scratch"], "probe-work", "events.sqlite"), "w") as out:
            out.write("task state of the %%s side\n" %% SIDE)
    else:
        campaign = os.path.join(os.path.dirname(request["scratch"]), "campaign")
        with open(os.path.join(campaign, "events.sqlite"), "a") as out:
            out.write("topped up to 10000 events by the %%s side\n" %% SIDE)
    sys.stdout.write('{"schema":"af.perf-probe/1","case_id":"%%s","correctness":{"status":'
                     '"passed","count":10000,"detail":"stand-in"}}\n' %% request["case_id"])
    sys.exit(0)
work = os.path.join(request["scratch"], "probe-work")
if request["case_id"].endswith("_seed"):
    # A native seed writer (ADR-0170): its side's fixture, written into its scratch, which is
    # the seed's own directory, and the message its golden seed entry declares.
    with open(os.path.join(CONTROL, "native-writes-" + SIDE), "a") as stream:
        stream.write(request["instance_id"] + "\n")
    if os.path.exists(os.path.join(CONTROL, "fail-native-seed-" + SIDE)):
        sys.stderr.write("the stand-in native writer fails\n")
        sys.exit(1)
    os.makedirs(os.path.join(work, "nested"))
    with open(os.path.join(work, "nested", "state"), "w") as out:
        out.write("%%s written by the %%s side\n" %% (request["instance_id"], SIDE))
    # Store bytes differ from one writing to the next, as a real Store's do: a seed's digest
    # identifies one written artifact, never the declared fixture.
    with open(os.path.join(work, "nested", "store-salt"), "w") as out:
        out.write(os.urandom(8).hex())
    escape = os.path.join(CONTROL, "symlink-native-seed-" + SIDE)
    if os.path.exists(escape):
        # A hostile writer leaves a symlink to a file outside its seed (ADR-0172).
        os.symlink(open(escape).read(), os.path.join(work, "nested", "escape"))
    entry = dict(ENTRIES[request["instance_id"]])
    if os.path.exists(os.path.join(CONTROL, "wrong-native-seed-" + SIDE)):
        entry["count"] -= 1
    message = {"schema": "af.perf-probe/1", "case_id": request["case_id"],
               "instance_id": request["instance_id"], "contract_id": request["contract_id"],
               "fixture_digest": None, "parameters": request["parameters"],
               "metrics": {"wall_ms": {"value": "1", "unit": "ms"},
                           "setup_ms": {"value": "1", "unit": "ms"}},
               "correctness": {"status": "passed", "digest": entry["digest"],
                               "count": entry["count"], "detail": "ok: stand-in writer"}}
    sys.stdout.write(json.dumps(message, sort_keys=True, separators=(",", ":")) + "\n")
    sys.exit(0)
if request["seed"] is not None:
    # A seed reader's own copy: a mark an earlier sample left would be shared state.
    state = os.path.join(request["seed"], "nested", "state")
    with open(state) as stream:
        data = stream.read()
    if "read by a sample" in data or SIDE + " side" not in data:
        sys.stderr.write("this seed copy is not this side's private copy\n")
        sys.exit(9)
    if os.path.exists(os.path.join(CONTROL, "tamper-native-seed-" + SIDE)):
        # Trusted local mode runs as the shard's user: the sealed seed is reachable.
        parent = os.path.dirname(os.path.dirname(request["scratch"]))
        for current, dirs, names in os.walk(parent):
            if os.path.basename(current).startswith(SIDE + "-") and "_seed__" in current:
                target = os.path.join(current, "probe-work", "nested", "state")
                os.chmod(target, 0o644)
                with open(target, "a") as out:
                    out.write("changed by a sample\n")
    with open(state, "a") as stream:
        stream.write("read by a sample\n")
entry = ENTRIES[request["instance_id"]]
counter = %(counter)r
calls = int(open(counter).read()) if os.path.exists(counter) else 0
with open(counter, "w") as stream:
    stream.write(str(calls + 1))
wall = "10.5"
if SLOW_AFTER is not None and calls >= SLOW_AFTER:
    wall = "40"
os.makedirs(os.path.join(request["scratch"], "probe-work"))
message = {"schema": "af.perf-probe/1", "case_id": request["case_id"],
           "instance_id": request["instance_id"], "contract_id": request["contract_id"],
           "fixture_digest": entry["fixture_digest"], "parameters": request["parameters"],
           "metrics": {"wall_ms": {"value": wall, "unit": "ms"},
                       "setup_ms": {"value": "2", "unit": "ms"}},
           "correctness": {"status": "passed", "digest": entry["digest"],
                           "count": entry["count"], "detail": "ok: stand-in probe"}}
sys.stdout.write(json.dumps(message, sort_keys=True, separators=(",", ":")) + "\n")
'''


def script(path, body):
    with open(path, "w") as stream:
        stream.write("#!%s -I\n%s" % (sys.executable, body))
    os.chmod(path, 0o755)
    return path


def authority_repo(path):
    """A repository whose commits hold the authority registry, golden entries and CLI fixtures
    exactly as this checkout has them, and a Cargo.lock recording af VERSION."""
    repo = Repo(path)
    files = {"Cargo.lock": LOCK, "README": "product v1\n"}
    for relative in ("fixtures/performance/suite.toml", "fixtures/performance/golden/native.json",
                     "fixtures/performance/cli.toml"):
        files[relative] = read(os.path.join(ROOT, relative))
    cli_root = os.path.join(ROOT, "fixtures/performance/cli")
    for current, _, names in os.walk(cli_root):
        for name in names:
            full = os.path.join(current, name)
            files[os.path.relpath(full, ROOT)] = read(full)
    base = repo.commit("base", files)
    head = repo.commit("head", {"README": "product v2\n"})
    return repo, base, head


def plan_for(repo, base, head, cases, run_id="runner_run", shards=1, environment=None):
    loaded = suite_module.load(SUITE)
    environment = environment or "macos_arm64_local"
    request = planning.Request(trigger="manual", tier="nightly", scope="partial_scope",
                               base=base, head=head, run_id=run_id, seed=11, cases=cases,
                               pairs=12, shards=shards, environment=environment,
                               trusted_host_build=environment != "macos_arm64_local")
    ids = planning.Identities()
    ids.baseline, ids.candidate, ids.control = base, head, base
    ids.trigger = {"kind": "manual", "remeasure_of": None}
    lock = sha256_bytes(LOCK.encode("utf-8"))
    digests = {"suite_sha256": loaded.raw_sha256, "policy_digest": loaded.policy_digest,
               "instrument_digest": sha256_bytes(b"instrument"),
               "fixture_digest": sha256_bytes(b"fixtures")}
    inputs = dict((role, {"cargo_lock_sha256": lock})
                  for role in ("authority", "baseline", "candidate"))
    return planning.freeze(loaded, request, ids, "control", base, None, digests, [], inputs)


def golden_entries():
    """Every golden instance's and seed writer's facts, by ID."""
    doc = json.loads(read(os.path.join(ROOT, "fixtures/performance/golden/native.json")))
    return dict((entry["instance_id"], {"fixture_digest": entry["fixture_digest"],
                                        "digest": entry["digest"], "count": entry["count"]})
                for entry in doc["entries"] + doc["seeds"])


def run_started_now(plan, started_unix_ms=None):
    """The run's execution as a build that started now (or at `started_unix_ms`) records it."""
    started = int(time.time() * 1000) if started_unix_ms is None else started_unix_ms
    total = plan["limits"]["run_total_minutes"]
    run = {"started_unix_ms": started, "deadline_unix_ms": started + total * 60000,
           "total_minutes": total}
    return {"run": run, "caller_deadline_unix_ms": None,
            "deadline_unix_ms": run["deadline_unix_ms"], "ended_unix_ms": started}


def make_build(root, plan, af_versions, slow_after=None, started_unix_ms=None):
    """A verified build directory of `plan` holding stand-in binaries under a manifest.

    The stand-ins keep their call counters, and the fake supervisor its `overrides.json`, in
    `root/control`, outside the hashed artifacts: the supervisor artifact is a wrapper that
    runs the fake from there, so a shard's rehash of every artifact still verifies."""
    artifacts = os.path.join(root, "artifacts")
    control = os.path.join(root, "control")
    os.makedirs(artifacts)
    os.makedirs(control)
    fake = fake_supervisor(os.path.join(control, "fake-supervisor"))
    entries = golden_entries()
    records = {}
    for role, name, body in (
            ("supervisor", "perf-supervisor", "import os, sys\nos.execv(%r, [%r])\n"
             % (fake, fake)),
            ("baseline", "af", FAKE_AF % {"version": af_versions["baseline"],
                                          "side": "baseline", "control": control}),
            ("baseline", "perf-probe", FAKE_PROBE % {
                "entries": entries, "slow_after": slow_after, "side": "baseline",
                "control": control,
                "counter": os.path.join(control, "baseline-probe-calls")}),
            ("candidate", "af", FAKE_AF % {"version": af_versions["candidate"],
                                           "side": "candidate", "control": control}),
            ("candidate", "perf-probe", FAKE_PROBE % {
                "entries": entries, "slow_after": None, "side": "candidate",
                "control": control,
                "counter": os.path.join(control, "candidate-probe-calls")})):
        os.makedirs(os.path.join(artifacts, role), exist_ok=True)
        path = os.path.join(artifacts, role, name)
        script(path, body)
        digest, size = builds.file_sha256(path)
        records[(role, name)] = {"path": "artifacts/%s/%s" % (role, name), "sha256": digest,
                                 "size": size}
    manifest = bundle.write_manifest(artifacts, 1 << 30)
    from perflab import synthetic
    doc = synthetic.builds(plan)
    doc["provenance"] = "measured"
    doc["supervisor"]["artifact"] = records[("supervisor", "perf-supervisor")]
    doc["supervisor"]["binary_sha256"] = records[("supervisor", "perf-supervisor")]["sha256"]
    for side in contracts.SIDES:
        af, probe = records[(side, "af")], records[(side, "perf-probe")]
        doc["sides"][side]["artifacts"] = {"af": af, "probe": probe}
        doc["sides"][side]["binaries"] = {"af": af["sha256"], "probe": probe["sha256"]}
    doc["artifacts"]["manifest_sha256"] = manifest
    doc["execution"] = run_started_now(plan, started_unix_ms)
    with open(os.path.join(root, builds.BUILDS_FILE), "wb") as stream:
        stream.write(contracts.write_bytes(doc, "builds"))
    return root


class ShardHarness(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.repo, self.base, self.head = authority_repo(self.path("checkout"))
        os.mkdir(self.path("scratch"), 0o700)
        os.mkdir(self.path("locks"), 0o755)

    def prepare(self, cases, af_versions=None, run_id="runner_run", shards=1, slow_after=None,
                overrides=None):
        plan = plan_for(self.repo, self.base, self.head, cases, run_id=run_id, shards=shards)
        plan_path = self.path(run_id + ".json")
        with open(plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        build = make_build(self.path(run_id + "-build"), plan,
                           af_versions or {"baseline": VERSION, "candidate": VERSION},
                           slow_after=slow_after)
        if overrides is not None:
            with open(os.path.join(build, "control", "overrides.json"), "w") as stream:
                json.dump(overrides, stream)
        return plan, plan_path, build

    def options(self, plan_path, build, shard=0, run_dir=None, **changes):
        values = dict(plan=plan_path, build=build, shard=shard,
                      run_dir=run_dir or self.path("run"), repo=self.repo.path,
                      mode=runner.TRUSTED_LOCAL, scratch=self.path("scratch"),
                      lock_directory=self.path("locks"), platform=("macos", "aarch64"))
        values.update(changes)
        return runner.Options(**values)

    def records(self, run_dir=None, shard=0):
        data = read(os.path.join(run_dir or self.path("run"), "shards", str(shard),
                                 "samples.jsonl"))
        return contracts.read_samples(data)

    def calls(self, build):
        path = os.path.join(build, "control", "supervisor-calls")
        return int(read(path)) if os.path.exists(path) else 0

    def assert_kept_one_namespace(self, doc):
        """The one private scratch a stopped shard kept for recovery, named by its record."""
        kept = [name for name in os.listdir(self.path("scratch"))
                if name.startswith(oracle.NAMESPACE_PREFIX)]
        self.assertEqual(len(kept), 1, kept)
        self.assertIn("kept for recovery: " + os.path.join(
            os.path.realpath(self.path("scratch")), kept[0]), doc["failure"]["detail"])
        return os.path.join(self.path("scratch"), kept[0])

    def assert_valid_evidence(self, verdict):
        """Complete, healthy, integral evidence; stand-in timings decide nothing else, so any
        reason left is a statistical classification of noise, never a coverage failure."""
        self.assertEqual(verdict["aggregate"]["coverage"]["missing_records"], 0)
        self.assertTrue(all(entry["status"] == "healthy" for entry in verdict["health"]),
                        verdict["health"])
        codes = set(reason["code"] for reason in verdict["aggregate"]["reasons"])
        self.assertTrue(codes <= {"inconclusive", "regression"}, codes)
        for case in verdict["cases"]:
            self.assertIsNotNone(case["interval"])



class Shards(ShardHarness):
    def test_a_shard_runs_every_planned_slot_in_order_and_aggregates(self):
        plan, plan_path, build = self.prepare([DISCOVERY, SMALL])
        before = sorted((name, os.stat(os.path.join(dirpath, name)).st_mtime_ns)
                        for dirpath, _, names in os.walk(os.path.join(build, "artifacts"))
                        for name in names)
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "complete", doc["failure"])
        self.assertEqual(doc["execution"]["mode"], "trusted_local")
        self.assertEqual(doc["execution"]["containment"], "process_group")
        self.assertIsNone(doc["execution"]["sample_image"])
        slots = schedule.expected_slots(plan, 0)
        records = self.records()
        # Every planned slot, sentinels at both ends, in planned sequence and AB/BA side.
        self.assertEqual([(r["sequence"], r["phase"], r["instance_id"], r["pair"],
                           r["position"], r["side"]) for r in records],
                         [(s["sequence"], s["phase"], s["instance_id"], s["pair"],
                           s["position"], s["side"]) for s in slots])
        phases = [r["phase"] for r in records]
        self.assertEqual(phases.count("sentinel_start"), 10)
        self.assertEqual(phases.count("sentinel_end"), 10)
        self.assertEqual(phases.count("warmup"), 8)
        # One supervisor invocation per record: nothing was retried or added.
        self.assertEqual(self.calls(build), len(records))
        for record in records:
            self.assertEqual(record["status"], "ok", record["failure"])
            self.assertEqual(record["host_id"], doc["host_id"])
            primary = record["metric_scopes"]["wall_ms"]
            if record["instance_id"] == DISCOVERY:
                self.assertEqual(primary, "child_process_lifetime")
                self.assertEqual(record["identity"]["adapter"], "af_cli_v1")
                self.assertEqual(record["oracle"]["kind"], "cli_version_and_usage_tokens")
                for key in ("user_cpu_ms", "cgroup_peak_memory_bytes"):
                    self.assertIn(key, record["unavailable_metrics"])
            else:
                self.assertEqual(primary, "library_region")
                self.assertEqual(record["oracle"]["detail"], "matches the golden entry")
            if record["phase"].startswith("sentinel"):
                self.assertEqual(record["side"], "baseline")
                self.assertEqual(record["identity"]["case_id"], "sentinel_canonical")
        if records[0]["instance_id"] == SMALL:
            self.assertEqual(records[0]["metric_scopes"]["setup_ms"], "probe_fixture_setup")
        # The build was only read: shards never rebuild or touch its artifacts.
        after = sorted((name, os.stat(os.path.join(dirpath, name)).st_mtime_ns)
                       for dirpath, _, names in os.walk(os.path.join(build, "artifacts"))
                       for name in names)
        self.assertEqual(before, after)
        # The scratch namespaces are gone; the private parent is untouched.
        self.assertEqual(os.listdir(self.path("scratch")), [])
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["provenance"], "measured")
        self.assertEqual(verdict["scope"], "partial_scope")
        self.assertFalse(verdict["aggregate"]["satisfies_required_check"])
        self.assert_valid_evidence(verdict)
        for unknown in ("cpu_steal", "cpu_pressure"):
            if doc["host"]["kernel"]["value"].startswith("Darwin"):
                self.assertIn("unknown", doc["observations"]["start"][unknown])

    def test_a_candidate_only_failure_fails_and_a_baseline_failure_is_incomplete(self):
        for wrong, expected, code in (("candidate", "fail", 3), ("baseline", "incomplete", 4)):
            with self.subTest(wrong=wrong):
                versions = {"baseline": VERSION, "candidate": VERSION}
                versions[wrong] = "9.9.9"
                run_id = "failure_" + wrong
                plan, plan_path, build = self.prepare([DISCOVERY], versions, run_id=run_id)
                run_dir = self.path("run-" + wrong)
                doc = runner.run_shard(self.options(plan_path, build, run_dir=run_dir))
                self.assertEqual(doc["status"], "complete")
                failed = [r for r in self.records(run_dir) if r["status"] == "failed"]
                self.assertTrue(failed)
                self.assertEqual(set(r["side"] for r in failed), {wrong})
                self.assertEqual(set(r["failure"]["kind"] for r in failed), {"oracle_mismatch"})
                self.assertIn("is 'af 9.9.9', not 'af %s'" % VERSION,
                              failed[0]["failure"]["detail"])
                self.assertTrue(failed[0]["logs"]["retained"].startswith("logs/"))
                # Failed samples are never retried.
                self.assertEqual(self.calls(build), len(self.records(run_dir)))
                verdict = compare.compare(run_dir)
                self.assertEqual(verdict["aggregate"]["result"], expected)
                self.assertEqual(verdict["aggregate"]["exit_code"], code)
                status = verdict["cases"][0]["status"]
                self.assertEqual(status, wrong + "_failure")

    def test_timeouts_and_leaked_descendants_fail_their_side_and_the_shard_continues(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="lifecycle",
                                              overrides={"12": "timeout", "13": "leak"})
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "complete")
        records = self.records()
        self.assertEqual(records[12]["failure"]["kind"], "timeout")
        self.assertIsNone(records[12]["execution"]["exit_code"])
        self.assertEqual(records[12]["execution"]["termination"], "timeout")
        self.assertNotIn("wall_ms", records[12]["metrics"])
        self.assertEqual(records[13]["failure"]["kind"], "lifecycle_violation")
        self.assertEqual(records[13]["execution"]["lifecycle"], "unexpected_descendants")
        self.assertEqual(doc["slots"]["failed"], 2)
        verdict = compare.compare(self.path("run"))
        self.assertNotEqual(verdict["aggregate"]["result"], "pass")
        kinds = sorted(item["kind"] for item in verdict["cases"][0]["failures"])
        self.assertEqual(kinds, ["lifecycle_violation", "timeout"])

    def test_an_uncertain_cleanup_stops_the_shard_and_leaves_slots_unstarted(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="uncertain",
                                              overrides={"14": "uncertain"})
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "failed")
        self.assertEqual(doc["failure"]["reason"], "cleanup_uncertain")
        # A process of the sample may still be live: its scratch is kept, never removed
        # beneath it, and named for recovery (ADR-0171).
        self.assert_kept_one_namespace(doc)
        self.assertEqual(doc["record_count"], 15)
        self.assertEqual(doc["slots"]["unstarted"], len(schedule.expected_slots(plan, 0)) - 15)
        self.assertEqual(self.calls(build), 15)
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["aggregate"]["result"], "incomplete")
        codes = set(reason["code"] for reason in verdict["aggregate"]["reasons"])
        self.assertTrue({"shard_not_complete", "missing_records"} <= codes, codes)

    def test_a_supervisor_without_a_reply_records_nothing_for_its_slot_and_stops(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="garbage",
                                              overrides={"3": "garbage"})
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "failed")
        # Without a valid reply nothing confirms that its sample ended (ADR-0171).
        self.assertEqual(doc["failure"]["reason"], "cleanup_uncertain")
        self.assertIn("perf-supervisor: invalid JSON", doc["failure"]["detail"])
        self.assert_kept_one_namespace(doc)
        self.assertEqual([r["sequence"] for r in self.records()], [0, 1, 2])
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["aggregate"]["result"], "incomplete")

    def test_an_artifact_changed_during_the_shard_stops_it_before_the_next_launch(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="tampered")
        target = os.path.join(build, "artifacts", "baseline", "af")
        with open(os.path.join(build, "control", "overrides.json"), "w") as out:
            json.dump({"21": "tamper:" + target}, out)
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "failed")
        self.assertEqual(doc["failure"]["reason"], "binary_changed")
        self.assertLessEqual(doc["record_count"], 23)

    def test_corrupted_health_marks_the_shard_unstable_and_keeps_every_number(self):
        # The baseline probe slows down from its 25th call, the first end sentinel (10 start
        # sentinel calls, then 14 of the case's 28 samples are the baseline's): the end
        # sentinels shift by far more than 20%, so the critical scope is environment_unstable.
        plan, plan_path, build = self.prepare([SMALL], run_id="unstable", slow_after=24)
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "complete")
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["health"][0]["status"], "unstable")
        self.assertIn("median_shift", verdict["health"][0]["flags"])
        case = verdict["cases"][0]
        self.assertEqual(case["status"], "environment_unstable")
        self.assertIsNotNone(case["interval"], "the numbers stay visible")
        self.assertEqual(verdict["aggregate"]["result"], "incomplete")

    def test_the_sampling_deadline_leaves_the_rest_unstarted_and_interrupted(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="deadline")
        # A deterministic clock that stands still until three samples have run.
        doc = runner.run_shard(self.options(
            plan_path, build, clock=lambda: 0.0 if self.calls(build) < 3 else 10.0 ** 6))
        self.assertEqual(doc["status"], "interrupted")
        self.assertEqual(doc["failure"]["reason"], "sampling_deadline")
        self.assertGreater(doc["slots"]["unstarted"], 0)
        self.assertEqual(doc["record_count"] + doc["slots"]["unstarted"],
                         doc["expected_records"])
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["aggregate"]["result"], "incomplete")

    def test_an_instance_without_a_runnable_cli_command_is_never_substituted(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="pending")
        doc_path = os.path.join(self.repo.path, "fixtures/performance/cli.toml")
        text = read(doc_path).decode("utf-8")
        start = text.index('[[adapters.af_cli_v1.commands]]\ncase_id = "cli_discovery"\n'
                           'parameters = { command = "version" }')
        end = text.index("[[adapters.af_cli_v1.commands]]", start + 10)
        text = text[:start] + ('[[adapters.af_cli_v1.commands]]\ncase_id = "cli_discovery"\n'
                               'parameters = { command = "version" }\nsetup = "none"\n'
                               'pending = "withheld by this test"\ndescription = "x"\n\n') \
            + text[end:]
        authority = self.repo.commit("authority", {"fixtures/performance/cli.toml": text})
        loaded = suite_module.load(SUITE)
        plan = dict(plan)
        plan["resolved"] = dict(plan["resolved"], authority_sha=authority)
        plan["plan_digest"] = contracts.plan_digest(plan)
        with open(plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        build = make_build(self.path("pending-build-2"), plan,
                           {"baseline": VERSION, "candidate": VERSION})
        self.assertEqual(loaded.policy_digest, plan["authority"]["policy_digest"])
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "failed")
        self.assertEqual(doc["failure"]["reason"], "unavailable_coverage")
        self.assertIn("withheld by this test", doc["failure"]["detail"])
        self.assertTrue(all(r["phase"].startswith("sentinel") for r in self.records()))
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["cases"][0]["status"], "missing_records")

    def test_hosts_modes_and_states_that_cannot_run_the_plan_refuse_before_sampling(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="refusals")
        with self.assertRaises(runner.Unavailable) as caught:
            runner.run_shard(self.options(plan_path, build, platform=("linux", "aarch64")))
        self.assertEqual(caught.exception.reason, "host_mismatch")
        with self.assertRaises(runner.Unavailable) as caught:
            runner.run_shard(self.options(plan_path, build, mode=runner.LINUX_CONTAINER))
        self.assertEqual(caught.exception.reason, "mode_mismatch")
        linux = plan_for(self.repo, self.base, self.head, ["source_capture"],
                         run_id="linux_plan", environment="linux_x86_64_musl_hosted")
        linux_path = self.path("linux.json")
        with open(linux_path, "wb") as stream:
            stream.write(contracts.write_bytes(linux, "run"))
        linux_build = make_build(self.path("linux-build"), linux,
                                 {"baseline": VERSION, "candidate": VERSION})
        # The Linux path is executable (ADR-0169); a host that cannot provide it refuses at
        # run time with its reason, before anything is created or sampled.
        for platform_, docker, reason in ((("macos", "aarch64"), "docker", "host_mismatch"),
                                          (("linux", "x86_64"), self.path("no-docker"),
                                           "no_container_runtime")):
            with self.subTest(reason=reason):
                with self.assertRaises(runner.Unavailable) as caught:
                    runner.run_shard(self.options(
                        linux_path, linux_build, run_dir=self.path("run-linux"),
                        mode=runner.LINUX_CONTAINER, platform=platform_, docker=docker,
                        image="sha256:" + "1" * 64, scratch_fs=self.path("scratch-fs")))
                self.assertEqual(caught.exception.reason, reason)
        # A frozen state that says not executable still refuses, whatever the host.
        withheld = json.loads(json.dumps(plan))
        withheld["cases"][0]["execution"] = {"state": "not_yet_executable",
                                             "pending": "withheld by this test"}
        withheld["plan_digest"] = contracts.plan_digest(withheld)
        withheld_path = self.path("withheld.json")
        with open(withheld_path, "wb") as stream:
            stream.write(contracts.write_bytes(withheld, "run"))
        withheld_build = make_build(self.path("withheld-build"), withheld,
                                    {"baseline": VERSION, "candidate": VERSION})
        with self.assertRaises(runner.Unavailable) as caught:
            runner.run_shard(self.options(withheld_path, withheld_build,
                                          run_dir=self.path("run-withheld")))
        self.assertEqual(caught.exception.reason, "not_executable")
        self.assertIn("withheld by this test", str(caught.exception))
        for path in ("run-linux", "run-withheld", "run"):
            self.assertFalse(os.path.exists(self.path(path)))

    def test_a_shard_runs_once_and_a_run_directory_belongs_to_one_run(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="once")
        runner.run_shard(self.options(plan_path, build))
        with self.assertRaises(ContractError) as caught:
            runner.run_shard(self.options(plan_path, build))
        self.assertIn("a shard runs once", str(caught.exception))
        other, other_path, other_build = self.prepare([DISCOVERY], run_id="other")
        with self.assertRaises(ContractError):
            runner.run_shard(self.options(other_path, other_build))

    def test_shards_from_separate_hosts_aggregate_into_one_verdict(self):
        plan, plan_path, build = self.prepare([DISCOVERY, SMALL], run_id="two_shards",
                                              shards=2)
        for index in (0, 1):
            runner.run_shard(self.options(plan_path, build, shard=index,
                                          run_dir=self.path("host-%d" % index)))
        partial = compare.compare(self.path("host-0"))
        self.assertEqual(partial["aggregate"]["result"], "incomplete")
        result = subprocess.run(
            [sys.executable, "-I", CLI, "aggregate", "--run", self.path("host-0"),
             "--shard-bundle", self.path("host-1", "shards", "1"),
             "--output", self.path("verdict.json")],
            cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertIn(result.returncode, (0, 3, 4), result.stderr.decode())
        verdict = json.loads(read(self.path("verdict.json")))
        self.assertEqual([entry["status"] for entry in verdict["shards"]],
                         ["complete", "complete"])
        self.assert_valid_evidence(verdict)
        # Importing the same bundle again changes nothing; a different one is refused.
        again = subprocess.run(
            [sys.executable, "-I", CLI, "aggregate", "--run", self.path("host-0"),
             "--shard-bundle", self.path("host-1", "shards", "1")],
            cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertIn(again.returncode, (0, 3, 4), again.stderr.decode())
        self.assertIn(b"already present", again.stderr)
        shutil.copytree(self.path("host-1", "shards", "1"), self.path("forged"))
        os.unlink(self.path("forged", "manifest.json"))
        with open(self.path("forged", "logs", "extra.log"), "w") as stream:
            stream.write("x")
        bundle.write_manifest(self.path("forged"), 1 << 30)
        refused = subprocess.run(
            [sys.executable, "-I", CLI, "aggregate", "--run", self.path("host-0"),
             "--shard-bundle", self.path("forged")],
            cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(refused.returncode, 2)
        self.assertIn(b"already holds a different bundle", refused.stderr)

    def test_the_run_command_refuses_an_ambiguous_mode_and_reports_unavailability(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="command")
        locks = self.path("command-locks")
        os.mkdir(locks, 0o755)
        bootstrap = ("import runpy,sys; sys.path.insert(0,%r); "
                     "from perflab import runner; original=runner.machine_lock; "
                     "runner.machine_lock=lambda directory=None,register=None,lease=None: "
                     "original(%r,register=register,lease=lease); "
                     "sys.argv=[%r]+sys.argv[1:]; runpy.run_path(%r,run_name='__main__')"
                     % (os.path.join(ROOT, "scripts", "performance"), locks, CLI, CLI))
        base = [sys.executable, "-I", "-B", "-c", bootstrap, "run", "--plan", plan_path,
                "--build", build, "--shard", "0", "--run-dir", self.path("cmd-run"),
                "--repo", self.repo.path]
        both = subprocess.run(base + ["--trusted-local", "--container"], cwd=ROOT,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(both.returncode, 2)
        self.assertFalse(os.path.exists(self.path("cmd-run")))
        container = subprocess.run(base + ["--container", "--image", "sha256:" + "1" * 64,
                                           "--scratch-fs", self.path("scratch")], cwd=ROOT,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(container.returncode, 4, container.stderr.decode())
        refusal = json.loads(container.stdout)
        self.assertIn(refusal["reason"], ("mode_mismatch", "host_mismatch"))
        # The lock it took is the private one.
        self.assertTrue(os.path.exists(os.path.join(locks, hostlock.MACHINE_LOCK_NAME)))


class SeedHarness(ShardHarness):
    def seeded(self, run_id, cases=(TASK_SHOW, LEDGER)):
        # Frozen on a Linux profile, then given the local macOS one so these stand-ins run in
        # trusted local mode; re-digested, as a plan is never edited in place otherwise.
        plan = plan_for(self.repo, self.base, self.head, list(cases), run_id=run_id,
                        environment="linux_x86_64_musl_hosted")
        plan["profile"] = plan_for(self.repo, self.base, self.head, [DISCOVERY],
                                   run_id=run_id)["profile"]
        for case in plan["cases"]:
            case["requires_linux_containment"] = False
        plan["plan_digest"] = contracts.plan_digest(plan)
        plan_path = self.path(run_id + ".json")
        with open(plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        build = make_build(self.path(run_id + "-build"), plan,
                           {"baseline": VERSION, "candidate": VERSION})
        return plan, plan_path, build

    def control(self, build, name):
        with open(os.path.join(build, "control", name), "w") as stream:
            stream.write("x")

    def writes(self, build, side):
        path = os.path.join(build, "control", "seed-writes-" + side)
        return read(path).decode().split() if os.path.exists(path) else []


class Seeds(SeedHarness):
    """Side-written seeds (ADR-0169): written once per side and seed by that side's own
    stand-in writers through the real setup.sh, sealed, verified around every sample, and
    copied privately into each sample; the stand-in AF fails a sample whose copy another sample
    already read. These shards run in trusted local mode with stand-in binaries; the contained
    path binds the same seeds (tests.test_supervise)."""

    def test_each_side_writes_each_seed_once_and_every_sample_copies_it_privately(self):
        plan, plan_path, build = self.seeded("seeded")
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "complete", doc["failure"])
        self.assertEqual(doc["execution"]["coordination_lock"],
                         os.path.join(os.path.realpath(self.path("locks")),
                                      "af-perf-machine.lock"))
        seeds = dict(((seed["side"], seed["seed"]), seed) for seed in doc["seeds"])
        self.assertEqual(sorted(seeds), [("baseline", "campaign_state"),
                                         ("baseline", "task_state"),
                                         ("candidate", "campaign_state"),
                                         ("candidate", "task_state")])
        for side in ("baseline", "candidate"):
            # Each writer ran exactly once for its side: nothing was rewritten per sample.
            self.assertEqual(sorted(self.writes(build, side)),
                             ["cli_campaign_fixture", "cli_state_fixture"])
        records = self.records()
        for (side, seed_id), seed in seeds.items():
            self.assertEqual(seed["status"], "ready", seed)
            self.assertTrue(seed["unchanged"])
            instance = TASK_SHOW if seed_id == "task_state" else LEDGER
            used = [r for r in records if r["instance_id"] == instance and r["side"] == side]
            self.assertEqual(seed["samples"], len(used))
            self.assertEqual(len(used), 14)
            for record in used:
                self.assertEqual(record["status"], "ok", record["failure"])
                self.assertEqual(record["identity"]["fixture_digest"], seed["digest"])
                self.assertIsNotNone(record["execution"]["fixture_setup_ns"])
        # Each side wrote its own state: no seed is shared across sides.
        self.assertNotEqual(seeds[("baseline", "task_state")]["digest"],
                            seeds[("candidate", "task_state")]["digest"])
        self.assertEqual(os.listdir(self.path("scratch")), [])
        self.assert_valid_evidence(compare.compare(self.path("run")))

    def test_a_failed_writer_fails_only_its_own_side_and_is_never_retried(self):
        plan, plan_path, build = self.seeded("seed_failure", cases=(TASK_SHOW,))
        self.control(build, "fail-seed-candidate")
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "complete", doc["failure"])
        seeds = dict(((seed["side"], seed["seed"]), seed) for seed in doc["seeds"])
        failed = seeds[("candidate", "task_state")]
        self.assertEqual((failed["status"], failed["failure"]["kind"], failed["samples"]),
                         ("failed", "launch_failure", 0))
        self.assertEqual(seeds[("baseline", "task_state")]["status"], "ready")
        self.assertEqual(self.writes(build, "candidate"), ["cli_state_fixture"])
        records = [r for r in self.records() if r["instance_id"] == TASK_SHOW]
        for record in records:
            if record["side"] == "candidate":
                self.assertEqual(record["failure"]["kind"], "launch_failure")
                self.assertIn("seed task_state was not written", record["failure"]["detail"])
            else:
                self.assertEqual(record["status"], "ok", record["failure"])
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["cases"][0]["status"], "candidate_failure")
        self.assertEqual(verdict["aggregate"]["result"], "fail")

    def test_a_changed_seed_stops_the_shard(self):
        plan, plan_path, build = self.seeded("seed_changed", cases=(TASK_SHOW,))
        self.control(build, "tamper-seed-baseline")
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "failed")
        self.assertEqual(doc["failure"]["reason"], "seed_changed")
        seed = [s for s in doc["seeds"] if s["side"] == "baseline"][0]
        self.assertFalse(seed["unchanged"])
        last = self.records()[-1]
        self.assertEqual((last["instance_id"], last["side"], last["status"]),
                         (TASK_SHOW, "baseline", "failed"))
        self.assertEqual(last["failure"]["kind"], "lifecycle_violation")
        # The seeds were removed even though the shard stopped.
        self.assertEqual(os.listdir(self.path("scratch")), [])
        self.assertEqual(compare.compare(self.path("run"))["aggregate"]["result"], "incomplete")


class NativeSeeds(SeedHarness):
    """Native seeds (ADR-0170): the cold and prewarmed projections share their side's one
    event-log seed, and each source size has its own repository seed. Each side's stand-in
    probe writes each seed once through its writer contract, whose check is judged against
    the golden seed entry; the shard seals the seed, verifies it around every sample, and every
    sample copies it privately in its untimed setup. The stand-in reader fails a copy another
    sample already read or another side wrote."""

    CASES = (COLD, WARM, SOURCE_100, SOURCE_10000)

    def native_writes(self, build, side):
        path = os.path.join(build, "control", "native-writes-" + side)
        return read(path).decode().split() if os.path.exists(path) else []

    def test_each_side_writes_each_native_seed_once_and_every_sample_copies_it(self):
        plan, plan_path, build = self.seeded("native", cases=self.CASES)
        self.assertEqual(sorted(set(case["seed"]["seed_id"] for case in plan["cases"])), [
            "source_capture_seed__files_10000_tree_source_files_128b",
            "source_capture_seed__files_100_tree_source_files_128b",
            "state_projection_seed__log_events_10000"])
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "complete", doc["failure"])
        seeds = dict(((seed["side"], seed["seed"]), seed) for seed in doc["seeds"])
        self.assertEqual(len(seeds), 6)
        for side in ("baseline", "candidate"):
            # One writer per seed and side; the cold and prewarmed projections share theirs.
            self.assertEqual(sorted(self.native_writes(build, side)),
                             sorted(set(case["seed"]["seed_id"] for case in plan["cases"])))
        records = self.records()
        for (side, seed_id), seed in seeds.items():
            self.assertEqual((seed["kind"], seed["status"], seed["af_sha256"]),
                             ("native", "ready", None), seed)
            self.assertTrue(seed["writer"].endswith("_seed_v1"))
            self.assertTrue(seed["unchanged"])
            # The seed's writing cost is recorded once, on the seed.
            self.assertIsNotNone(seed["writer_wall_ns"])
            readers = [case["instance_id"] for case in plan["cases"]
                       if case["seed"]["seed_id"] == seed_id]
            used = [r for r in records if r["instance_id"] in readers and r["side"] == side]
            self.assertEqual(seed["samples"], len(used))
            self.assertEqual(len(used), 14 * len(readers))
            for record in used:
                self.assertEqual(record["status"], "ok", record["failure"])
                self.assertEqual(record["identity"]["fixture_digest"], seed["digest"])
                # Each copy is setup timed apart from the region: the supervisor's setup, then
                # the probe's own setup, never the primary metric.
                self.assertIsNotNone(record["execution"]["fixture_setup_ns"])
                self.assertEqual(record["metric_scopes"]["wall_ms"], "library_region")
                self.assertEqual(record["metric_scopes"]["setup_ms"], "probe_fixture_setup")
                # The primary metric is the probe's region alone: the stand-in's 10.5 ms,
                # whatever the copy before it took.
                self.assertEqual(record["metrics"]["wall_ms"]["value"], "10.5")
        self.assertNotEqual(seeds[("baseline", "state_projection_seed__log_events_10000")]
                            ["digest"],
                            seeds[("candidate", "state_projection_seed__log_events_10000")]
                            ["digest"])
        # Every seed was removed with the shard.
        self.assertEqual(os.listdir(self.path("scratch")), [])
        verdict = compare.compare(self.path("run"))
        self.assert_valid_evidence(verdict)

        # A record that names no ready native seed of its own side is refused.
        root = os.path.join(self.path("run"), "shards", "0")

        def forget_candidate_seeds(shard):
            shard["seeds"] = [seed for seed in shard["seeds"] if seed["side"] == "baseline"]
        from perflab import synthetic
        synthetic.reseal_shard(root, forget_candidate_seeds)
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["aggregate"]["result"], "incomplete")
        self.assertIn("read no copy of the", " ".join(
            reason["detail"] for reason in verdict["aggregate"]["reasons"]))

    def test_a_failed_or_wrong_native_writer_fails_only_its_side_and_is_never_retried(self):
        for control, kind in (("fail-native-seed-candidate", "fixture_failed"),
                              ("wrong-native-seed-candidate", "oracle_mismatch")):
            with self.subTest(kind=kind):
                plan, plan_path, build = self.seeded("native_" + kind, cases=(COLD, WARM))
                self.control(build, control)
                run_dir = self.path("run-" + kind)
                doc = runner.run_shard(self.options(plan_path, build, run_dir=run_dir))
                self.assertEqual(doc["status"], "complete", doc["failure"])
                seeds = dict((seed["side"], seed) for seed in doc["seeds"])
                self.assertEqual((seeds["candidate"]["status"],
                                  seeds["candidate"]["failure"]["kind"],
                                  seeds["candidate"]["samples"]), ("failed", kind, 0))
                self.assertEqual(seeds["baseline"]["status"], "ready")
                self.assertEqual(len(self.native_writes(build, "candidate")), 1)
                for record in self.records(run_dir):
                    if record["instance_id"] not in (COLD, WARM):
                        continue
                    if record["side"] == "candidate":
                        self.assertEqual(record["failure"]["kind"], "launch_failure")
                        self.assertIn("was not written", record["failure"]["detail"])
                    else:
                        self.assertEqual(record["status"], "ok", record["failure"])
                self.assertEqual(compare.compare(run_dir)["aggregate"]["result"], "fail")
                self.assertEqual(os.listdir(self.path("scratch")), [])

    def test_a_seed_holding_a_symlink_is_refused_before_anything_outside_it_changes(self):
        # Root-reproduced (F4): sealing followed a seed's symlink and changed an owned file
        # outside the seed from 0600 to 0555 before the tree refused it.
        outside = self.path("outside-marker")
        with open(outside, "wb") as stream:
            stream.write(b"owned bytes outside every seed\n")
        os.chmod(outside, 0o600)
        outside_dir = self.path("outside-dir")
        os.mkdir(outside_dir, 0o700)
        for target in (outside, outside_dir):
            with self.subTest(target=os.path.basename(target)):
                run_id = "native_escape_" + os.path.basename(target).replace("-", "_")
                plan, plan_path, build = self.seeded(run_id, cases=(COLD,))
                with open(os.path.join(build, "control", "symlink-native-seed-candidate"),
                          "w") as stream:
                    stream.write(target)
                run_dir = self.path("run-" + run_id)
                doc = runner.run_shard(self.options(plan_path, build, run_dir=run_dir))
                self.assertEqual(doc["status"], "complete", doc["failure"])
                seeds = dict((seed["side"], seed) for seed in doc["seeds"])
                self.assertEqual(seeds["baseline"]["status"], "ready")
                candidate = seeds["candidate"]
                self.assertEqual((candidate["status"], candidate["failure"]["kind"]),
                                 ("failed", "fixture_failed"))
                self.assertIn("nested/escape, which is a symlink",
                              candidate["failure"]["detail"])
                # Nothing outside the seed was read, changed or removed.
                self.assertEqual(read(outside), b"owned bytes outside every seed\n")
                self.assertEqual(os.stat(outside).st_mode & 0o777, 0o600)
                self.assertEqual(os.stat(outside_dir).st_mode & 0o777, 0o700)
                for record in self.records(run_dir):
                    if record["instance_id"] == COLD and record["side"] == "candidate":
                        self.assertEqual(record["failure"]["kind"], "launch_failure")
                # The seed, symlink included, was removed with the shard.
                self.assertEqual(os.listdir(self.path("scratch")), [])
                self.assertEqual(compare.compare(run_dir)["aggregate"]["result"], "fail")

    def test_a_plan_seed_other_than_the_registry_s_is_refused_before_anything_runs(self):
        plan, plan_path, build = self.seeded("native_identity", cases=(COLD,))
        plan["cases"][0]["seed"] = dict(plan["cases"][0]["seed"],
                                        contract_id="state_projection_seed_v2")
        plan["plan_digest"] = contracts.plan_digest(plan)
        with open(plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        build = make_build(self.path("native_identity-rebuilt"), plan,
                           {"baseline": VERSION, "candidate": VERSION})
        with self.assertRaises(ContractError) as caught:
            runner.run_shard(self.options(plan_path, build))
        self.assertIn("is not the authority registry's", str(caught.exception))
        self.assertFalse(os.path.exists(self.path("run")))
        self.assertEqual(self.calls(build), 0)

    def test_a_changed_native_seed_stops_the_shard_and_is_removed(self):
        plan, plan_path, build = self.seeded("native_changed", cases=(SOURCE_100,))
        self.control(build, "tamper-native-seed-baseline")
        doc = runner.run_shard(self.options(plan_path, build))
        self.assertEqual(doc["status"], "failed")
        self.assertEqual(doc["failure"]["reason"], "seed_changed")
        seed = [s for s in doc["seeds"] if s["side"] == "baseline"][0]
        self.assertFalse(seed["unchanged"])
        last = self.records()[-1]
        self.assertEqual((last["instance_id"], last["side"], last["failure"]["kind"]),
                         (SOURCE_100, "baseline", "lifecycle_violation"))
        self.assertEqual(os.listdir(self.path("scratch")), [])
        self.assertEqual(compare.compare(self.path("run"))["aggregate"]["result"], "incomplete")


class RunDeadline(ShardHarness):
    """The run's one execution start and total deadline come from its build (ADR-0170): a
    delayed shard never resets them, a caller's deadline only tightens them, and slots whose
    worst case would pass the deadline stay unstarted."""

    def build_started(self, run_id, started_unix_ms):
        plan = plan_for(self.repo, self.base, self.head, [DISCOVERY], run_id=run_id)
        plan_path = self.path(run_id + ".json")
        with open(plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(plan, "run"))
        build = make_build(self.path(run_id + "-build"), plan,
                           {"baseline": VERSION, "candidate": VERSION},
                           started_unix_ms=started_unix_ms)
        return plan, plan_path, build

    def test_a_shard_started_after_the_run_deadline_starts_nothing(self):
        plan, plan_path, build = self.build_started(
            "late", int(time.time() * 1000) - plan_minutes() * 60000 - 1000)
        with self.assertRaises(runner.Unavailable) as caught:
            runner.run_shard(self.options(plan_path, build))
        self.assertEqual(caught.exception.reason, "run_deadline")
        self.assertFalse(os.path.exists(self.path("run")))
        self.assertEqual(self.calls(build), 0)
        # A caller's deadline that has passed refuses the same way.
        plan, plan_path, build = self.build_started("caller", int(time.time() * 1000))
        with self.assertRaises(runner.Unavailable) as caught:
            runner.run_shard(self.options(plan_path, build,
                                          deadline_unix_ms=int(time.time() * 1000) - 1))
        self.assertEqual(caught.exception.reason, "run_deadline")

    def test_the_run_deadline_of_a_delayed_shard_leaves_its_rest_unstarted(self):
        # The build started long enough ago that 300 s of the run remain; a deterministic
        # clock advances 40 s per sample, so the worst case of a later slot passes it.
        now = int(time.time() * 1000)
        started = now - plan_minutes() * 60000 + 300000
        plan, plan_path, build = self.build_started("delayed", started)
        doc = runner.run_shard(self.options(plan_path, build,
                                            clock=lambda: 40.0 * self.calls(build)))
        self.assertEqual(doc["status"], "interrupted")
        self.assertEqual(doc["failure"]["reason"], "run_deadline")
        execution = doc["execution"]
        self.assertEqual(execution["run"]["started_unix_ms"], started)
        self.assertEqual(execution["deadline_unix_ms"], execution["run"]["deadline_unix_ms"])
        self.assertGreater(doc["slots"]["unstarted"], 0)
        self.assertGreater(doc["record_count"], 0)
        verdict = compare.compare(self.path("run"))
        self.assertEqual(verdict["aggregate"]["result"], "incomplete")
        self.assertIn("shard_not_complete", [r["code"] for r in verdict["aggregate"]["reasons"]])
        self.assertNotIn("run_deadline_exceeded",
                         [r["code"] for r in verdict["aggregate"]["reasons"]])

    def test_a_caller_deadline_only_tightens_the_run_s(self):
        now = int(time.time() * 1000)
        plan, plan_path, build = self.build_started("tightened", now)
        caller = now + 200000
        doc = runner.run_shard(self.options(plan_path, build, deadline_unix_ms=caller,
                                            clock=lambda: 40.0 * self.calls(build)))
        self.assertEqual(doc["status"], "interrupted")
        self.assertEqual(doc["execution"]["caller_deadline_unix_ms"], caller)
        self.assertEqual(doc["execution"]["deadline_unix_ms"], caller)
        self.assertEqual(doc["execution"]["run"]["deadline_unix_ms"],
                         now + plan_minutes() * 60000)
        # A later caller deadline never extends the run's.
        plan, plan_path, build = self.build_started("extended", now)
        doc = runner.run_shard(self.options(plan_path, build, run_dir=self.path("run2"),
                                            deadline_unix_ms=now + 10 ** 9))
        self.assertEqual(doc["status"], "complete", doc["failure"])
        self.assertEqual(doc["execution"]["deadline_unix_ms"], min(
            doc["execution"]["run"]["deadline_unix_ms"],
            doc["started_unix_ms"] + doc["execution"]["sampling_deadline_ms"]))


class HostCancellation(SeedHarness):
    """A supervisor the host must cancel (ADR-0171): the run's deadline either refuses a
    launch it cannot cancel in time or cancels the sample through its supervisor; a supervisor
    that never confirms its sample's end stops the shard, and everything its sample could reach
    is kept, never removed beneath a process that may still be live. Deterministic clocks stand
    still at 0 for the deadline's reading and at 57 s after it, 60 s being the deadline, so
    every launch has exactly 3 s left whatever the real preflight took. The wall clock stands
    still too, at the instant the caller's deadline was fixed 60 s before, so the deadline the
    shard maps onto its monotonic clock never shrinks by the preflight's real duration."""

    def clock(self):
        readings = []

        def read_clock():
            readings.append(None)
            return 0.0 if len(readings) == 1 else 57.0
        return read_clock

    def short(self):
        """Seams for these tests only: a slot's worst case and the terminal reserve of 1 s,
        a 0.1 s cleanup grace and a 0.5 s cancellation margin, so nothing waits production
        minutes; the production values are untouched outside them."""
        return [mock.patch.object(runner.Shard, "worst_case_seconds", lambda self, slot: 1),
                mock.patch.object(runner, "FINISH_RESERVE_SECONDS", 1)]

    def slow_preflight(self, seconds=1.5):
        """A preflight that takes real time before the deadline is read, as on a loaded gate
        host: with coherent fixture clocks the launch still has exactly its 3 s."""
        actual = runner.read_build_manifest

        def read_build_manifest(build, plan):
            time.sleep(seconds)
            return actual(build, plan)
        return mock.patch.object(runner, "read_build_manifest", read_build_manifest)

    def run_patched(self, patches, plan_path, build, **changes):
        for patch in patches:
            patch.start()
        try:
            now_ms = int(time.time() * 1000)
            return runner.run_shard(self.options(
                plan_path, build, clock=self.clock(), wall=lambda: now_ms / 1000.0,
                deadline_unix_ms=now_ms + 60000, **changes))
        finally:
            for patch in patches:
                patch.stop()

    def test_a_launch_the_deadline_cannot_cancel_in_time_is_refused_and_interrupts(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="refused_launch")
        doc = self.run_patched(self.short(), plan_path, build)
        self.assertEqual((doc["status"], doc["failure"]["reason"]),
                         ("interrupted", "run_deadline"))
        self.assertIn("nothing was launched", doc["failure"]["detail"])
        self.assertEqual((doc["record_count"], self.calls(build)), (0, 0))
        self.assertEqual(os.listdir(self.path("scratch")), [])

    def test_a_supervisor_that_never_confirms_its_sample_s_end_keeps_its_scratch(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="deaf",
                                              overrides={"0": "deaf"})
        patches = self.short() + [mock.patch.object(runner, "GRACE_MS", 100),
                                  mock.patch.object(supervise, "CANCEL_MARGIN_SECONDS", 0.5),
                                  self.slow_preflight()]
        marker = os.path.join(build, "control", "deaf-child")
        started = time.monotonic()
        try:
            doc = self.run_patched(patches, plan_path, build)
            self.assertLess(time.monotonic() - started, 60)
            self.assertEqual((doc["status"], doc["failure"]["reason"]),
                             ("failed", "cleanup_uncertain"))
            # Exactly 3 s left and a 0.8 s cancellation reserve.
            self.assertIn("within 2.200 s, the run's total deadline", doc["failure"]["detail"])
            self.assertIn("gave no reply within 0.800 s and was killed",
                          doc["failure"]["detail"])
            kept = self.assert_kept_one_namespace(doc)
            # The owned child is still running in the scratch that was kept.
            os.kill(int(read(marker)), 0)
            self.assertTrue(os.path.isdir(kept))
            self.assertEqual((doc["record_count"], self.calls(build)), (0, 1))
        finally:
            end_owned_group(marker)

    def test_a_seed_writer_that_never_confirms_its_end_keeps_the_seed_store(self):
        plan, plan_path, build = self.seeded("deaf_seed", cases=(COLD,))
        with open(os.path.join(build, "control", "overrides.json"), "w") as stream:
            json.dump({"native_seed_writer": "deaf"}, stream)
        patches = self.short() + [mock.patch.object(runner, "GRACE_MS", 100),
                                  mock.patch.object(supervise, "CANCEL_MARGIN_SECONDS", 0.5),
                                  self.slow_preflight()]
        marker = os.path.join(build, "control", "deaf-child")
        try:
            doc = self.run_patched(patches, plan_path, build)
            self.assertEqual((doc["status"], doc["failure"]["reason"]),
                             ("failed", "cleanup_uncertain"))
            seed = doc["seeds"][0]
            self.assertEqual((seed["status"], seed["failure"]["kind"]), ("failed", "driver"))
            # The writer's scratch and the shard's seed store, which a live writer could still
            # write, are both kept and named.
            kept = sorted(name for name in os.listdir(self.path("scratch")))
            self.assertEqual(len(kept), 2, kept)
            for name in kept:
                self.assertIn(os.path.join(os.path.realpath(self.path("scratch")), name),
                              doc["failure"]["detail"])
            os.kill(int(read(marker)), 0)
        finally:
            end_owned_group(marker)


class Preflight(ShardHarness):
    """The run's dominant deadline is carried through the preflight (ADR-0171): a Linux
    shard's container runtime query and scratch enforcement fill are bounded by the time left
    before it less the terminal reserve, and no later step starts after it. Controlled clocks
    and stand-in capabilities; no Docker or fill runs."""

    def setUp(self):
        ShardHarness.setUp(self)
        self.plan = plan_for(self.repo, self.base, self.head, ["source_capture"],
                             run_id="preflight", environment="linux_x86_64_musl_hosted")
        self.plan_path = self.path("preflight.json")
        with open(self.plan_path, "wb") as stream:
            stream.write(contracts.write_bytes(self.plan, "run"))
        self.build = make_build(self.path("preflight-build"), self.plan,
                                {"baseline": VERSION, "candidate": VERSION})
        self.now = [0.0]
        self.steps = []

    def shard(self, remaining_ms, docker_seconds=0.0, fill=None, real_check=False):
        def docker_info(docker, timeout):
            self.steps.append(("docker", timeout))
            self.now[0] += docker_seconds
            return {"identity": "linux x86_64 cgroup2 27.0", "unavailable": None}

        actual = runner.capability.scratch_capability

        def scratch_capability(path, limit, verify_enforcement, host, deadline):
            # One absolute instant of the shard's own clock, fixed before the check starts.
            self.assertEqual(host.clock(), self.now[0])
            self.steps.append(("fill", deadline - self.now[0]))
            if real_check:
                return actual(path, limit, verify_enforcement, "linux", host=host,
                              deadline=deadline)
            if fill is not None:
                return fill
            raise AssertionError("stop here: the fill is the step under test")

        options = self.options(
            self.plan_path, self.build, run_dir=self.path("run-linux"),
            mode=runner.LINUX_CONTAINER, platform=("linux", "x86_64"),
            image="sha256:" + "1" * 64, scratch_fs=self.path("scratch-fs"),
            clock=lambda: self.now[0],
            deadline_unix_ms=int(time.time() * 1000) + remaining_ms)
        with mock.patch.object(runner.capability, "docker_info", docker_info), \
                mock.patch.object(runner.capability, "scratch_capability", scratch_capability):
            return runner.Shard(options, self.path("locks/lock"))

    def test_an_expired_deadline_after_a_slow_runtime_query_starts_no_fill(self):
        # Root-reproduced: 31 s remained, 1 s beyond the terminal reserve; the runtime query
        # took 2 s, and the fill used to begin after the deadline had passed.
        with self.assertRaises(runner.Unavailable) as caught:
            self.shard(31000, docker_seconds=2.0)
        self.assertEqual(caught.exception.reason, "run_deadline")
        self.assertIn("verifying the scratch filesystem's bound", str(caught.exception))
        self.assertEqual([step for step, _ in self.steps], ["docker"])
        self.assertLessEqual(self.steps[0][1], 1.0)
        self.assertFalse(os.path.exists(self.path("run-linux")))

    def test_every_step_is_bounded_by_the_time_left_before_the_deadline(self):
        with self.assertRaises(AssertionError):
            self.shard(100000)
        (docker, query), (fill, seconds) = self.steps
        self.assertAlmostEqual(query, 30.0, places=0)
        # 100 s left less the 30 s terminal reserve, far below the fill's own 600 s bound.
        self.assertGreater(seconds, 60.0)
        self.assertLessEqual(seconds, 70.0)

    def test_a_fill_the_deadline_ended_is_the_deadline_s_never_an_unverified_scratch(self):
        record = {"capability": {"status": "unavailable",
                                 "reasons": ["enforcement not verified: the fill did not "
                                             "finish within its time bound"]},
                  "fill": {"stopped": "deadline"}, "path": self.path("scratch-fs")}
        with self.assertRaises(runner.Unavailable) as caught:
            self.shard(100000, fill=record)
        self.assertEqual(caught.exception.reason, "run_deadline")
        self.assertIn("ended verifying the scratch filesystem's bound", str(caught.exception))
        # The same record within the fill's own bound stays the scratch's refusal.
        self.steps = []
        with self.assertRaises(runner.Unavailable) as caught:
            self.shard(10 ** 7, fill=record)
        self.assertEqual(caught.exception.reason, "scratch_unverified")

    def test_a_slow_check_before_the_fill_starts_no_fill_and_is_the_deadline_s(self):
        # Root-reproduced (ADR-0173): 31 s remained, one beyond the terminal reserve, and
        # finding the scratch mount took two; the fill used to start after the deadline with
        # its whole relative allowance. The actual check now runs within the instant the shard
        # fixed before it, so nothing after the slow step starts.
        calls = []
        now = self.now

        class SlowMount(runner.capability.Host):
            def realpath(self, path):
                return path

            def mount_of(self, path, mountinfo):
                calls.append("mount_of")
                now[0] += 2.0
                return (path, "ext4")

            def statvfs(self, path):
                calls.append("statvfs")
                raise AssertionError("statvfs after the deadline")

            def open(self, path, flags, mode):
                calls.append("open")
                raise AssertionError("a fill after the deadline")

        with mock.patch.object(runner.capability, "Host", SlowMount):
            with self.assertRaises(runner.Unavailable) as caught:
                self.shard(31000, real_check=True)
        self.assertEqual(caught.exception.reason, "run_deadline")
        self.assertIn("ended verifying the scratch filesystem's bound", str(caught.exception))
        self.assertEqual(calls, ["mount_of"])
        (_, query), (_, seconds) = self.steps
        self.assertLessEqual(seconds, 1.0)
        self.assertFalse(os.path.exists(self.path("run-linux")))

    def test_no_preflight_step_starts_once_the_deadline_leaves_only_the_reserve(self):
        with self.assertRaises(runner.Unavailable) as caught:
            self.shard(29000)
        self.assertEqual(caught.exception.reason, "run_deadline")
        # The first step is the artifacts' admission (ADR-0172); nothing after it ran.
        self.assertIn("verifying the build's artifacts", str(caught.exception))
        self.assertEqual(self.steps, [])

    def hashed_shard(self, remaining_ms, seconds_per_hash=2.0):
        """The actual constructor over the actual tiny build, where only each artifact hash's
        elapsed time is controlled: every hash advances the shard's clock (ADR-0172)."""
        hashed = []
        original = runner.builds.bundle._hash_file

        def timed_hash(path, limit, label, budget=None):
            hashed.append(os.path.basename(path))
            result = original(path, limit, label, budget)
            self.now[0] += seconds_per_hash
            return result

        with mock.patch.object(runner.builds.bundle, "_hash_file", timed_hash):
            try:
                self.shard(remaining_ms)
            finally:
                self.hashed = hashed

    def test_artifact_admission_never_starts_without_the_dominant_deadline_s_time(self):
        # Root-reproduced: 0.5 s remained; all five artifact hashes used to run, four after the
        # caller's deadline, before the deadline refused.
        with self.assertRaises(runner.Unavailable) as caught:
            self.hashed_shard(500)
        self.assertEqual(caught.exception.reason, "run_deadline")
        self.assertIn("verifying the build's artifacts", str(caught.exception))
        self.assertIn("it was not started", str(caught.exception))
        self.assertEqual(self.hashed, [])
        self.assertEqual(self.steps, [])
        self.assertFalse(os.path.exists(self.path("run-linux")))

    def test_the_deadline_stops_artifact_hashing_while_it_runs(self):
        # 35 s remain, 5 s beyond the terminal reserve: three 2-s hashes fit, the fourth is
        # refused before its first read, and the refusal is the deadline's.
        with self.assertRaises(runner.Unavailable) as caught:
            self.hashed_shard(35000)
        self.assertEqual(caught.exception.reason, "run_deadline")
        self.assertIn("ended hashing build/artifacts/", str(caught.exception))
        self.assertEqual(len(self.hashed), 4)
        self.assertEqual(self.steps, [])
        self.assertFalse(os.path.exists(self.path("run-linux")))
        # With time for every hash, admission completes and the next step runs.
        self.now[0] = 0.0
        with self.assertRaises(AssertionError):
            self.hashed_shard(100000, seconds_per_hash=0.0)
        self.assertEqual(len(self.hashed), 5)
        self.assertEqual([step for step, _ in self.steps], ["docker", "fill"])

    def test_a_tampered_artifact_is_still_refused_within_the_deadline(self):
        target = os.path.join(self.build, "artifacts", "candidate", "af")
        os.chmod(target, 0o755)
        with open(target, "ab") as stream:
            stream.write(b"# changed\n")
        with self.assertRaises(ContractError) as caught:
            self.shard(100000)
        self.assertIn("build/artifacts", str(caught.exception))
        self.assertEqual(self.steps, [])


class SamplingDeadline(ShardHarness):
    """Every binary rehash and seed read during sampling stays within the shard's dominant
    deadline (ADR-0172): once it leaves no time beyond the terminal reserve, the work stops as
    the deadline's interruption, never as a changed artifact or seed."""

    def test_rehashes_and_seed_reads_stop_at_the_dominant_deadline(self):
        plan, plan_path, build = self.prepare([DISCOVERY], run_id="sampling_deadline")
        shard = runner.Shard(self.options(plan_path, build), self.path("locks/lock"))
        try:
            now = [0.0]
            spent = runner.budget.Budget(10.0, lambda: now[0], runner.FINISH_RESERVE_SECONDS)
            shard.sampling_budget = spent
            shard.bound = "sampling_deadline"
            with self.assertRaises(runner.Stop) as caught:
                shard.verified(shard.binaries.supervisor)
            self.assertEqual((caught.exception.reason, caught.exception.status),
                             ("sampling_deadline", "interrupted"))
            seed_root = self.path("seed")
            os.makedirs(os.path.join(seed_root, "probe-work"))
            with open(os.path.join(seed_root, "probe-work", "state"), "w") as stream:
                stream.write("state")
            seed = runner.Seed("baseline", "task_state", "cli", "seed_task_state", None,
                               "sha256:" + "0" * 64)
            seed.path, seed.digest = seed_root, "sha256:" + "1" * 64
            with self.assertRaises(runner.Stop) as caught:
                shard.check_seed(seed, "before sequence 0")
            self.assertEqual((caught.exception.reason, caught.exception.status),
                             ("sampling_deadline", "interrupted"))
            self.assertIn("verifying the baseline side's seed task_state",
                          caught.exception.detail)
            self.assertTrue(seed.record["unchanged"], "an unread seed is not called changed")
            # With time left, the same rehash passes and a changed seed is still caught.
            shard.sampling_budget = runner.budget.Budget(1000.0, lambda: now[0],
                                                         runner.FINISH_RESERVE_SECONDS)
            self.assertEqual(shard.verified(shard.binaries.supervisor),
                             shard.binaries.supervisor[0])
            with self.assertRaises(runner.Stop) as caught:
                shard.check_seed(seed, "before sequence 0")
            self.assertEqual(caught.exception.reason, "seed_changed")
        finally:
            runner._remove_work(shard.work)


def end_owned_group(marker):
    """End the process group a fixture started and recorded in `marker`; nothing else."""
    try:
        pid = int(read(marker))
    except (OSError, ValueError):
        return
    try:
        os.killpg(pid, 9)
    except OSError:
        return
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except OSError:
            return
        time.sleep(0.01)


def plan_minutes():
    """The nightly tier's frozen run_total_minutes, which these plans take."""
    return suite_module.load(SUITE).doc["tiers"]["nightly"]["run_total_minutes"]


class CliRegistry(TempDir):
    def test_the_committed_registry_covers_every_process_instance(self):
        doc = clicases.load(os.path.join(ROOT, "fixtures/performance/cli.toml"))
        loaded = suite_module.load(SUITE)
        clicases.check_registry(doc, loaded.doc)
        adapter = doc["adapters"]["af_cli_v1"]
        runnable = sorted("%s %s" % (entry["case_id"], json.dumps(entry["parameters"],
                                                                  sort_keys=True))
                          for entry in adapter["commands"] if "pending" not in entry)
        self.assertEqual(len(runnable), 9)
        self.assertEqual([entry for entry in adapter["commands"] if "pending" in entry], [])
        # The commands the design requires exist and use the help that exists: task show
        # --json, review ledger's semantic text (it has no --json flag) and review report
        # --format json, each over a private copy of the side's own seed.
        argv = dict((entry["parameters"].get("command", entry["case_id"]), entry["argv"])
                    for entry in adapter["commands"] if "argv" in entry)
        self.assertIn("--json", argv["task_show"])
        self.assertEqual(argv["review_ledger"][:3], ["{af}", "review", "ledger"])
        self.assertNotIn("--json", argv["review_ledger"])
        self.assertEqual(argv["review_report"][-2:], ["--format", "json"])
        seeds = dict((entry["parameters"]["command"], (entry["setup"], entry["seed"]))
                     for entry in adapter["commands"] if entry["case_id"] == "cli_state_read")
        self.assertEqual(seeds, {"task_show": ("seed_copy", "task_state"),
                                 "review_ledger": ("seed_copy", "campaign_state"),
                                 "review_report": ("seed_copy", "campaign_state")})
        self.assertEqual(sorted(doc["seeds"]), ["campaign_state", "task_state"])
        for seed in doc["seeds"].values():
            self.assertEqual(seed["events"], 10000)
        self.assertEqual(argv["cli_task_run_check"][:4], ["{af}", "task", "run",
                                                          "perf-check-true"])
        edited = json.loads(json.dumps(doc))
        edited["adapters"]["af_cli_v1"]["commands"].pop(0)
        with self.assertRaises(ContractError) as caught:
            clicases.check_registry(edited, loaded.doc)
        self.assertIn("no command for cli_discovery__command_version", str(caught.exception))

    def test_the_task_fixture_catalog_pins_its_packages_by_the_reference_digest(self):
        repo = os.path.join(ROOT, "fixtures/performance/cli/task-roots/repo")
        self.assertEqual(clicases.catalog_problems(repo), [])
        # The committed fixture may be read-only in a sealed checkout; only the test's own
        # copy is made writable, never the source.
        copy = writable_copy(repo, self.path("repo"))
        path = os.path.join(copy, ".af/task-packages/perf/accept/worker.py")
        with open(path, "a") as stream:
            stream.write("# edited\n")
        problems = clicases.catalog_problems(copy)
        self.assertEqual(len(problems), 1)
        self.assertIn("perf/accept pins", problems[0])
        campaign = os.path.join(ROOT, "fixtures/performance/cli/campaign-roots/repo")
        self.assertEqual(clicases.pipeline_problems(campaign), [])
        copy = writable_copy(campaign, self.path("campaign"))
        with open(os.path.join(copy, ".af/pipelines/review.toml"), "a") as stream:
            stream.write("# edited\n")
        problems = clicases.pipeline_problems(copy)
        self.assertEqual(len(problems), 1)
        self.assertIn("pipeline review pins", problems[0])
        task = json.loads(read(os.path.join(ROOT, "fixtures/performance/cli/task-roots/"
                                            "task.json")))
        doc = clicases.load(os.path.join(ROOT, "fixtures/performance/cli.toml"))
        self.assertEqual(task["task_id"], doc["task_fixture"]["task_id"])

    def test_each_side_s_version_comes_from_its_own_bound_cargo_lock(self):
        self.assertEqual(clicases.package_version(LOCK.encode("utf-8")), VERSION)
        with self.assertRaises(ContractError):
            clicases.package_version(b'version = 4\n')
        doc = clicases.load(os.path.join(ROOT, "fixtures/performance/cli.toml"))
        self.assertEqual(clicases.choose_adapter(doc, VERSION)[0], "af_cli_v1")
        self.assertEqual(clicases.choose_adapter(doc, "0.13.0")[0], "af_cli_v2")
        self.assertIsNone(clicases.choose_adapter(doc, "0.12.0-rc.2")[0])
        self.assertIsNone(clicases.choose_adapter(doc, "0.11.9")[0])
        repo, base, head = authority_repo(self.path("checkout"))
        plan = plan_for(repo, base, head, [DISCOVERY])
        from perflab import gitrefs
        self.assertEqual(clicases.side_versions(gitrefs.Git(repo.path), plan),
                         {"baseline": VERSION, "candidate": VERSION})
        forged = dict(plan, build_inputs=dict(plan["build_inputs"], candidate={
            "cargo_lock_sha256": sha256_bytes(b"another lock")}))
        with self.assertRaises(ContractError) as caught:
            clicases.side_versions(gitrefs.Git(repo.path), forged)
        self.assertIn("not the one the plan bound", str(caught.exception))


class CliOracle(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        doc = clicases.load(os.path.join(ROOT, "fixtures/performance/cli.toml"))
        adapter = doc["adapters"]["af_cli_v1"]
        self.commands = dict((entry["parameters"].get("command", entry["case_id"]), entry)
                             for entry in adapter["commands"])
        self.values = {"af": "/af", "probe": "/probe", "scratch": "/s",
                       "package_version": VERSION}

    def judge(self, name, stdout, returncode=0, truncated=0, counts=None, whole=None):
        """Judge `stdout` as retained, `truncated` bytes past it unretained; the supervisor's
        counts and JSON observation are those over `whole`, the complete output, when given,
        as the test-only stand-in of tests.support observes them."""
        entry = self.commands[name]
        whole = stdout if whole is None else whole
        if counts is None:
            counts = [whole.count(pattern.encode("utf-8"))
                      for pattern in clicases.patterns(entry)]
        selectors = clicases.selectors(entry)
        observed = observe_json(selectors, whole) if selectors else None
        return clicases.judge(entry, self.values, returncode, stdout, truncated, counts,
                              observed)

    def test_discovery_checks_the_side_s_version_and_usage_tokens_only(self):
        self.assertTrue(self.judge("version", b"af 0.12.0-rc.3\n")[0])
        for stdout, why in ((b"af 0.12.0\n", "not 'af 0.12.0-rc.3'"),
                            (b"af 0.12.0-rc.3\nextra\n", "holds 2 lines"),
                            (b"", "empty output never passes")):
            passed, detail, _ = self.judge("version", stdout)
            self.assertFalse(passed)
            self.assertIn(why, detail)
        help_text = (b"Usage: af task <COMMAND>\n\nCommands:\n  start  Capture\n"
                     b"  run    Run\n  show   Show\n")
        self.assertTrue(self.judge("task_help", help_text)[0])
        self.assertFalse(self.judge("task_help", help_text.replace(b"  show", b"  shew"))[0])
        passed, detail, _ = self.judge("task_help", help_text, returncode=2)
        self.assertFalse(passed)
        self.assertIn("exited 2, not 0", detail)

    def test_versioned_top_level_help_requires_its_heading_usage_and_commands(self):
        doc = clicases.load(os.path.join(ROOT, "fixtures/performance/cli.toml"))
        for version, adapter_id, heading in (
                ("0.12.0-rc.3", "af_cli_v1", "Commands:"),
                ("0.12.0-rc.5", "af_cli_v1", "Commands:"),
                ("0.13.0-rc.1", "af_cli_v2", "Commands, in the order a first run needs them:"),
                ("0.13.0-rc.2", "af_cli_v2", "Commands, in the order a first run needs them:"),
                ("0.13.0", "af_cli_v2", "Commands, in the order a first run needs them:")):
            with self.subTest(version=version):
                chosen, adapter = clicases.choose_adapter(doc, version)
                self.assertEqual(chosen, adapter_id)
                entry = clicases.command(adapter, "cli_discovery", {"command": "help"})
                text = ("Usage: af [OPTIONS] [COMMAND]\n\n" + heading +
                        "\n  review  Review\n  task  Task\n\nOptions:\n  --version  Version\n").encode()
                values = dict(self.values, package_version=version)

                def judge(output, code=0):
                    return clicases.judge(entry, values, code, output, 0, [], None)[0]

                self.assertTrue(judge(text))
                for missing in (b"Usage: af [OPTIONS] [COMMAND]", b"  review ",
                                b"  task ", b"--version"):
                    self.assertFalse(judge(text.replace(missing, b"broken")), missing)
                for malformed in ("", "Commands", "Commandz:", "Unexpected " + heading,
                                  heading + " unexpected", "Commands:" if adapter_id == "af_cli_v2"
                                  else "Commands, in the order a first run needs them:"):
                    self.assertFalse(judge(text.replace(heading.encode(), malformed.encode())),
                                     malformed)
                self.assertFalse(judge(text, 2))
                version_entry = clicases.command(adapter, "cli_discovery", {"command": "version"})
                self.assertTrue(clicases.judge(version_entry, values, 0,
                    ("af " + version + "\n").encode(), 0, [], None)[0])
                self.assertFalse(clicases.judge(version_entry, values, 0,
                    b"af 0.0.0\n", 0, [], None)[0])

    def test_task_outcomes_need_the_expected_state_and_the_executed_check(self):
        run = {"task_id": "perf-check-true", "phase": {"kind": "finished"},
               "result": {"execution": "completed", "acceptance": "satisfied",
                          "domain_conclusion": "verified"},
               "run_reports": [{"report": {"nodes": [
                   {"node": "root.inputs", "outcome": {"kind": "completed"}},
                   {"node": "root.nodes.checks", "outcome": {"kind": "completed"}}]}}],
               "runtime_observations": [{"record": {"node": "root.nodes.checks", "spans": [
                   {"kind": "check", "label": "truth", "elapsed_ms": 2}]}}]}
        self.assertTrue(self.judge("cli_task_run_check", json.dumps(run).encode())[0])
        unexecuted = json.loads(json.dumps(run))
        unexecuted["runtime_observations"][0]["record"]["spans"] = []
        passed, detail, _ = self.judge("cli_task_run_check", json.dumps(unexecuted).encode())
        self.assertFalse(passed)
        self.assertIn('"label": "truth"', detail)
        unverified = json.loads(json.dumps(run))
        unverified["result"]["domain_conclusion"] = "unverified"
        self.assertFalse(self.judge("cli_task_run_check", json.dumps(unverified).encode())[0])
        passed, detail, _ = self.judge("cli_task_run_check", json.dumps(run).encode()[:40],
                                       truncated=100)
        self.assertFalse(passed)
        self.assertIn("truncated", detail)
        start = {"task_id": "perf-check-true", "phase": {"kind": "ready"},
                 "plan_id": "sha256:" + "a" * 64, "attempts": 0,
                 "history": [{"transition": {"change": {"kind": "opened"}}},
                             {"transition": {"change": {"kind": "plan_proposed"}}}]}
        self.assertTrue(self.judge("cli_task_start", json.dumps(start).encode())[0])
        start["attempts"] = True
        self.assertFalse(self.judge("cli_task_start", json.dumps(start).encode())[0],
                         "a boolean is not the integer 0")

    # The task show oracle (ADR-0172). Its observation comes from the complete synthetic
    # output, as the supervisor observes it; the judge reads only the retained prefix and the
    # truncation, as a shard does. The retained 196,608 bytes are the sample log's default
    # 256 KiB less its stderr quarter. The real supervisor's observation of the same documents
    # is tested in crates/review-process/tests/perf_supervisor.rs.
    RETAINED = 196608

    def events(self, numbers, kinds=None):
        return [{"sequence": number, "transition": {"change": {
            "kind": (kinds or {}).get(index, ("opened", "plan_proposed", "plan_admitted")[index]
                                      if index < 3 else "lease_renewed")}}}
                for index, number in enumerate(numbers)]

    def task_show(self, events, **fields):
        document = {"schema": "x", "task_id": "perf-probe-task", "history": events,
                    "execution_records": [], "run_reports": []}
        document.update(fields)
        return json.dumps(document, separators=(",", ":")).encode()

    def judge_show(self, whole, retained=None):
        retained = self.RETAINED if retained is None else retained
        return self.judge("task_show", whole[:retained],
                          truncated=max(0, len(whole) - retained), whole=whole)

    def test_state_counts_are_exact_over_output_larger_than_any_retained_log(self):
        document = self.task_show(self.events(range(10000)))
        self.assertGreater(len(document), 512 * 1024)
        self.assertTrue(self.judge_show(document)[0])
        passed, detail, _ = self.judge_show(self.task_show(self.events(range(9999))))
        self.assertFalse(passed)
        self.assertIn("history.*.sequence holds 9999 values numbered 0..9998, not 10000 "
                      "numbered 0..9999", detail)

    def test_the_task_history_must_be_exactly_0_to_9999_once_and_in_order(self):
        # Root-reproduced: 10,000 entries with only four distinct numbers, and 10,000 unique
        # numbers with the middle reversed, both passed af.perf-cli/3's counts (ADR-0171); a
        # fractional or exponent number, entries moved into an unrelated array, and a document
        # without its closing brackets passed af.perf-cli/4's prefix observation (ADR-0172).
        numbers = list(range(10000))
        four = [0, 1, 2] + [3] * 9996 + [9999]
        reversed_middle = numbers[:100] + numbers[100:9900][::-1] + numbers[9900:]
        swapped = numbers[:5000] + [5001, 5000] + numbers[5002:]
        refused = (
            ("four distinct numbers", self.task_show(self.events(four)),
             "value 5 of history.*.sequence is 3 after 3"),
            ("the middle reversed", self.task_show(self.events(reversed_middle)),
             "is 9899 after 99"),
            ("one adjacent swap", self.task_show(self.events(swapped)), "is 5001 after 4999"),
            ("a duplicate", self.task_show(self.events(numbers[:5000] + [4999]
                                                       + numbers[5000:9999])),
             "is 4999 after 4999"),
            ("a missing entry", self.task_show(self.events(numbers[:5000] + numbers[5001:]
                                                           + [10000])),
             "is 5001 after 4999"),
            ("an extra entry", self.task_show(self.events(numbers + [10000])),
             "holds 10001 values numbered 0..10000"),
            ("starting at one", self.task_show(self.events(range(1, 10001))),
             "numbered 1..10000, not 10000 numbered 0..9999"),
            ("in reverse", self.task_show(self.events(numbers[::-1])), "is 9998 after 9999"),
        )
        whole = self.task_show(self.events(numbers))
        middle = b'{"sequence":5000,'
        moved = json.loads(whole)
        moved["unrelated"], moved["history"] = moved["history"][1:], moved["history"][:1]
        refused += (
            ("a fraction", whole.replace(middle, b'{"sequence":5000.5,'),
             "value 5001 of history.*.sequence is not a plain nonnegative integer"),
            ("an exponent", whole.replace(middle, b'{"sequence":5000e1,'),
             "value 5001 of history.*.sequence is not a plain nonnegative integer"),
            ("a string", whole.replace(middle, b'{"sequence":"5000",'),
             "value 5001 of history.*.sequence is not a plain nonnegative integer"),
            ("entries moved into an unrelated array",
             json.dumps(moved, separators=(",", ":")).encode(),
             "holds 1 values numbered 0..0, not 10000"),
            ("an entry without its number",
             self.task_show(self.events(numbers) + [{"transition": {}}]),
             "10001 values hold history.*.sequence's parent, but 10000 hold its member"),
            ("a repeated member", whole.replace(middle, b'{"sequence":5000,"sequence":5000,'),
             "history.*.sequence is named 1 more times than once in one object"),
            ("no closing brackets", whole[:-2], "stdout is not one complete JSON document "
             "(incomplete"),
            ("trailing content", whole + b"{}", "stdout is not one complete JSON document"),
        )
        for name, document, why in refused:
            with self.subTest(name):
                passed, detail, _ = self.judge_show(document)
                self.assertFalse(passed, name)
                self.assertIn(why, detail)
        self.assertTrue(self.judge_show(whole)[0])

    def test_the_task_identity_and_renewals_are_selected_semantics(self):
        whole = self.task_show(self.events(range(10000)))
        passed, detail, _ = self.judge_show(self.task_show(self.events(range(10000)),
                                                           task_id="another-task"))
        self.assertFalse(passed)
        self.assertIn('rule 2 (json_stream_equals): task_id reaches 1 values, not exactly one '
                      '"perf-probe-task"', detail)
        repeated = whole[:-1] + b',"task_id":"perf-probe-task"}'
        passed, detail, _ = self.judge_show(repeated)
        self.assertFalse(passed)
        self.assertIn("task_id is named 1 more times", detail)
        renewals = self.task_show(self.events(range(10000), kinds={5000: "lease_lost"}))
        passed, detail, _ = self.judge_show(renewals)
        self.assertFalse(passed)
        self.assertIn('history.*.transition.change.kind is "lease_renewed" 9996 times, not '
                      '9997', detail)

    def test_the_history_is_judged_whole_at_every_retention_and_chunk_boundary(self):
        document = self.task_show(self.events(range(10000)))
        marker = b'{"sequence":5000,'
        middle = document.index(marker)
        # Retained prefixes that end before, inside and just after an entry's number, at the
        # default log split, and with nothing truncated at all: the whole stream decides.
        for retained in (64 * 1024, self.RETAINED, middle, middle + 5, middle + len(marker),
                         len(document) - 1, len(document)):
            with self.subTest(retained=retained):
                self.assertTrue(self.judge_show(document, retained)[0])
                broken = document.replace(marker, b'{"sequence":5001,')
                passed, detail, _ = self.judge_show(broken, retained)
                self.assertFalse(passed)
                self.assertIn("is 5001 after 4999", detail)
        # Formatting the product documents freely: whitespace, member order and fields or
        # values no rule declares do not matter (ADR-0172).
        spaced = json.dumps(json.loads(document), indent=2).encode()
        self.assertTrue(self.judge_show(spaced)[0], "equivalent whitespace passes")
        reordered = json.loads(document)
        reordered["history"] = [dict(reversed(list(entry.items())))
                                for entry in reordered["history"]]
        reordered = dict(reversed(list(reordered.items())))
        self.assertTrue(self.judge_show(json.dumps(reordered).encode())[0],
                        "reordered members pass")
        fields = self.task_show(self.events(range(10000)), phase={"kind": "anything"},
                                execution_records=[{"note": "free"}],
                                late=[{"sequence": 10000}])
        self.assertTrue(self.judge_show(fields)[0])

    def test_a_sequence_follows_the_seed_s_declared_events(self):
        doc = clicases.load(os.path.join(ROOT, "fixtures/performance/cli.toml"))
        changed = json.loads(json.dumps(doc))
        for command in changed["adapters"]["af_cli_v1"]["commands"]:
            if command["parameters"].get("command") == "task_show":
                for rule in command["rules"]:
                    if rule["kind"] == "json_stream_sequence":
                        rule["last"] = 9998
        with self.assertRaises(ContractError) as caught:
            clicases.check_registry(changed, suite_module.load(SUITE).doc)
        self.assertIn("numbers 0..9998, not the seed task_state's 10000 events",
                      str(caught.exception))

    # The Campaign oracles check every declared Finding (ADR-0170). The counterexamples are
    # synthetic outputs, never observed product failures: each passed all nine rules of
    # af.perf-cli/2.

    SUMMARY = {"open": 0, "pending_verification": 0, "fixed": 0, "rejected": 0, "wontfix": 0,
               "contested": 0}

    def finding(self, index, **changes):
        finding = {"key": "sha256:%064x" % index, "aliases": [], "status": "open",
                   "severity": "minor", "last_seen_round": 1, "source": "fixture",
                   "identity_file": "src/fixture.txt", "identity_line": index,
                   "file": "src/fixture.txt", "line": index,
                   "title": "Fixture finding %d" % index, "body": "fixture", "fix": "none",
                   "confidence": 0.5}
        finding.update(changes)
        return finding

    def report(self, findings, **summary):
        document = {"schema": "af/review-report@4", "campaign": "perf-campaign",
                    "runs_recorded": 1, "ledger_round": 1, "final_verdict": "pass",
                    "rounds": [], "task_accounting": [], "demands": [],
                    "findings_summary": dict(self.SUMMARY, **summary), "findings": findings}
        return json.dumps(document).encode()

    def test_the_report_oracle_checks_every_finding_and_its_summary(self):
        findings = [self.finding(index) for index in range(1, 101)]
        self.assertTrue(self.judge("review_report", self.report(findings, open=100))[0])
        # Documented fields the fixture does not declare are never frozen: other bodies,
        # confidences, aliases, Round bookkeeping and key order still pass.
        loose = [dict(reversed(list(self.finding(index, body="anything", confidence=None,
                                                 aliases=["sha256:" + "f" * 64]).items())))
                 for index in range(1, 101)]
        self.assertTrue(self.judge("review_report", self.report(loose, open=100))[0])
        refused = (
            # 99 fixed Findings and one open "Fixture finding 100", the summary saying 100 open.
            ([self.finding(index, status="fixed") for index in range(1, 100)]
             + [self.finding(100)], {"open": 100}, "item 0 is no declared item"),
            # The declared Finding 100, a hundred times.
            ([self.finding(100) for _ in range(100)], {"open": 100}, "item 100 appears 100"),
            # Two Findings sharing one key, and a Finding missing while another repeats.
            ([self.finding(index) for index in range(1, 100)]
             + [self.finding(100, key="sha256:%064x" % 1)], {"open": 100},
             "two items share one key"),
            ([self.finding(index) for index in range(1, 100)]
             + [self.finding(99, key="sha256:" + "e" * 64)], {"open": 100},
             "item 99 appears 2 times"),
            # A changed status, severity or location.
            ([self.finding(index, status="contested" if index == 7 else "open")
              for index in range(1, 101)], {"open": 100}, "item 6 is no declared item"),
            ([self.finding(index, severity="major" if index == 3 else "minor")
              for index in range(1, 101)], {"open": 100}, "item 2 is no declared item"),
            ([self.finding(index, line=index + 1 if index == 5 else index)
              for index in range(1, 101)], {"open": 100}, "item 4 is no declared item"),
            # A summary inconsistent with the Findings it summarizes.
            ([self.finding(index) for index in range(1, 101)], {"open": 100, "fixed": 3},
             "fixed counts 3, the items 0"),
        )
        for findings, summary, why in refused:
            with self.subTest(why=why):
                passed, detail, _ = self.judge("review_report", self.report(findings,
                                                                             **summary))
                self.assertFalse(passed)
                self.assertIn(why, detail)

    def ledger(self, rows):
        return "".join("%s\tminor\t%s\tin\tminor\tsrc/fixture.txt:%d\tFixture finding %d\n"
                       % (key, status, line, title) for key, status, line, title in rows
                       ).encode()

    def test_the_ledger_oracle_checks_every_finding_line(self):
        rows = [("sha256:%064x" % index, "open", index, index) for index in range(1, 101)]
        self.assertTrue(self.judge("review_ledger", self.ledger(rows))[0])
        refused = (
            # 99 copies of Finding 1 and Finding 100: lines, counts and both ends all match.
            ([rows[0]] * 99 + [rows[99]], "item 1 appears 99 times"),
            ([rows[99]] * 100, "item 100 appears 100 times"),
            (rows[:99] + [("sha256:%064x" % 1, "open", 100, 100)], "two items share one key"),
            (rows[:6] + [(rows[6][0], "fixed", 7, 7)] + rows[7:], "line 7 does not match"),
            (rows[:4] + [(rows[4][0], "open", 5, 6)] + rows[5:], "line 5 does not match"),
            (rows[:99] + [("sha256:%064x" % 101, "open", 101, 101)], "outside 1..100"),
        )
        for broken, why in refused:
            with self.subTest(why=why):
                passed, detail, _ = self.judge("review_ledger", self.ledger(broken))
                self.assertFalse(passed)
                self.assertIn(why, detail)
        # Output not retained whole cannot show every item.
        entry = self.commands["review_ledger"]
        partial = dict(entry, rules=[rule for rule in entry["rules"]
                                     if rule["kind"] == "stdout_enumerates"]
                       + [{"kind": "exit_code", "value": 0}])
        passed, detail, _ = clicases.judge(partial, self.values, 0, self.ledger(rows)[:-1], 0,
                                           [], [])
        self.assertFalse(passed)
        self.assertIn("not retained whole", detail)


if __name__ == "__main__":
    import unittest
    unittest.main()


class HostObservations(TempDir):
    class LinuxSource(object):
        system = "Linux"
        FILES = {
            "/proc/cpuinfo": "processor\t: 0\nmodel name\t: Fixture CPU @ 3.0GHz\n"
                             "microcode\t: 0xf0\nflags\t\t: fpu hypervisor\n",
            "/proc/meminfo": "MemTotal:        2048 kB\n",
            "/etc/os-release": 'NAME="Fixture"\nPRETTY_NAME="Fixture Linux 1"\n',
            "/proc/self/cgroup": "0::/perf.slice\n",
            "/sys/fs/cgroup/perf.slice/cpu.max": "max 100000\n",
            "/sys/fs/cgroup/perf.slice/cpu.stat": "usage_usec 5\nnr_throttled 0\n",
            "/proc/sys/kernel/randomize_va_space": "2\n",
            "/proc/stat": "cpu  1 2 3 4 5 6 7 8 9 10\n",
            "/proc/pressure/cpu": "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
        }

        def read(self, path):
            if path in self.FILES:
                return self.FILES[path]
            raise OSError(2, "No such file or directory: " + path)

        def run(self, argv):
            raise OSError("no tools in this fixture")

        def listdir(self, path):
            raise OSError(2, "No such file or directory: " + path)

        def affinity(self):
            return [0, 2]

        def loadavg(self):
            return (0.5, 0.25, 0.125)

    def test_every_observation_is_a_value_or_a_reasoned_unknown(self):
        from perflab import host
        source = self.LinuxSource()
        identity = host.identity("docker 27; image sha256:x", source)
        contracts.HOST.validate(identity, "host")
        self.assertEqual(identity["cpu_model"], {"value": "Fixture CPU @ 3.0GHz"})
        self.assertEqual(identity["memory_bytes"], {"value": "2097152"})
        self.assertEqual(identity["microcode"], {"value": "0xf0"})
        self.assertEqual(identity["affinity"], {"value": "0,2"})
        self.assertEqual(identity["cpu_quota"], {"value": "max 100000"})
        self.assertEqual(identity["aslr"], {"value": "randomize_va_space=2"})
        self.assertEqual(identity["os_image"], {"value": "Fixture Linux 1"})
        self.assertIn("hypervisor flag set", identity["virtualization"]["value"])
        # Unreadable sources are unknown with their reason, never an invented zero.
        self.assertIn("unknown", identity["cpu_topology"])
        self.assertIn("No such file", identity["cpu_topology"]["unknown"])
        self.assertIn("unknown", identity["cache"])
        load = host.load(source)
        contracts.LOAD.validate(load, "load")
        self.assertEqual(load["load_average"], {"value": "0.50 0.25 0.12"})
        self.assertEqual(load["cpu_steal"], {"value": "steal_jiffies=8"})
        self.assertIn("avg10", load["cpu_pressure"]["value"])
        self.assertIn("No such file", load["memory_pressure"]["unknown"])
        self.assertIn("nr_throttled 0", load["cpu_throttling"]["value"])
        self.assertIn("unknown", load["memory_events"])
        # The shard's host identity is exactly the hash of these observations.
        self.assertEqual(contracts.host_id(identity), contracts.host_id(dict(identity)))
