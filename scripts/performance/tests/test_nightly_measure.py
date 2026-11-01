"""The nightly measurement (scripts/nightly/measure.py) and the systemd chain's units
(scripts/performance/service/af-nightly*, ADR-0273) with injected fakes.

A night's record is written into a temporary builds root from the nightly-build fixtures; the
nightly executor is a fake runner that answers `submit`, `status` and `cycle` as told and
records every argument list, and the clock is a fake that the fake sleep advances. No test
starts the executor, systemd, Docker or cargo, or waits in real time. The unit files are read
as text.
"""
import contextlib
import importlib.util
import io
import json
import os
import subprocess
import sys

from perflab import contracts, tomlfile
from perflab.nightlycontracts import DEADLINE_MINUTES_MAX
from tests.support import ROOT, TempDir, read

MEASURE = os.path.join(ROOT, "scripts", "nightly", "measure.py")
_spec = importlib.util.spec_from_file_location("nightly_measure", MEASURE)
measure = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(measure)

SERVICE = os.path.join(ROOT, "scripts", "performance", "service")
SCHEMA = os.path.join(ROOT, "fixtures", "performance", "schema")
CLI = os.path.join(ROOT, "scripts", "performance", "cli.py")
TAG = "nightly-2026.10.29"
HEAD = "b" * 40
CORE, TELEMETRY = "exp_" + "c" * 48, "exp_" + "e" * 48
STARTED_MS = 1793314800000  # 2026-10-29T23:00:00Z, an hour after the timer


class Clock:
    """Unix milliseconds that move only when the measurement sleeps."""

    def __init__(self, now=STARTED_MS):
        self.now = now
        self.slept = []

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.slept.append(seconds)
        self.now += int(seconds * 1000)


class Executor:
    """The nightly executor's command line: `states` and `cycles` are the answers of each
    `status` and `cycle` in turn (the last one repeats); a refusal is a (reason, detail) pair,
    answered as cli.py answers it, exit 4 and its refusal document on stdout."""

    def __init__(self, test, states, cycles=("complete",), submits=None):
        self.test = test
        self.states = list(states)
        self.cycles = list(cycles)
        self.submits = list(submits or [])
        self.calls = []

    def answer(self, queue):
        return queue.pop(0) if len(queue) > 1 else queue[0]

    def __call__(self, argv):
        self.calls.append(list(argv))
        self.test.assertEqual(argv[:7], [sys.executable, "-I", "-B", CLI, "executor", "--config",
                                         self.test.executor_config])
        operation = argv[7]
        if operation == "submit":
            answer = self.submits.pop(0) if self.submits else None
            if isinstance(answer, tuple):
                return refusal(*answer)
            work = argv[argv.index("--work") + 1]
            ident = CORE if work == "core" else TELEMETRY
            out = {"request_id": ident, "submission": {"kind": "nightly", "tag": TAG,
                                                       "key": "nightly:%s:%s" % (TAG, work)}}
        elif operation == "status":
            answer = self.answer(self.states)
            if isinstance(answer, tuple):
                return refusal(*answer)
            core, telemetry = answer.split("/")
            out = [{"request_id": CORE, "state": core}, {"request_id": TELEMETRY,
                                                         "state": telemetry},
                   {"request_id": "exp_" + "0" * 48, "state": "complete"}]
        else:
            self.test.assertEqual(operation, "cycle")
            answer = self.answer(self.cycles)
            if isinstance(answer, tuple):
                return refusal(*answer)
            return subprocess.CompletedProcess(argv, 0, (answer + "\n").encode())
        return subprocess.CompletedProcess(argv, 0, json.dumps(out).encode())

    def operations(self):
        return [call[7] for call in self.calls]


def refusal(reason, detail):
    doc = {"schema": "af.perf-refusal/1", "status": "unavailable", "reason": reason,
           "detail": detail}
    return subprocess.CompletedProcess([], 4, json.dumps(doc).encode())


def submit(work):
    return [sys.executable, "-I", "-B", CLI, "executor", "--config", None, "submit", "--work",
            work, "--kind", "nightly", "--tag", TAG, "--head", HEAD]


class Night(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.builds = self.path("builds")
        self.executor_config = self.path("executor.toml")
        self.config = self.path("nightly.toml")
        self.write_config()
        self.clock = Clock()

    def write_config(self, report=True, **measure_values):
        top = {"repository": "owner/af", "mirror": self.path("mirror.git"),
               "builds_root": self.builds, "build_image": "sha256:" + "1" * 64,
               "qemu_aarch64": self.path("qemu"), "sign_key": self.path("nightly.key"),
               "public_key": self.path("nightly.pub"), "required_checks": ["check / lint"]}
        lines = ['schema = "af.nightly-config/1"']
        lines += ["%s = %s" % (key, json.dumps(value)) for key, value in top.items()]
        if measure_values:
            lines.append("[measure]")
            lines += ["%s = %s" % item for item in measure_values.items()]
        if report:
            lines += ["[report]", "executor_config = %s" % json.dumps(self.executor_config),
                      'headline_instances = ["synthetic_fast"]']
        with open(self.config, "w") as stream:
            stream.write("\n".join(lines) + "\n")

    def night(self, name="valid-built"):
        record = json.loads(read(os.path.join(SCHEMA, "nightly-build", name + ".json")))
        os.makedirs(os.path.join(self.builds, record["date"]))
        with open(os.path.join(self.builds, record["date"], "nightly.json"), "wb") as stream:
            stream.write(contracts.write_bytes(record, "nightly-build"))
        return record

    def measure(self, executor, *extra):
        out, err = io.StringIO(), io.StringIO()
        argv = ["submit", "--config", self.config] + list(extra)
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = measure.main(argv, run=executor, clock=self.clock, sleep=self.clock.sleep)
        return code, out.getvalue(), err.getvalue()

    def submits(self):
        return [[self.executor_config if word is None else word for word in submit(work)]
                for work in ("core", "telemetry")]


class BuiltNight(Night):
    def test_both_originals_are_submitted_then_cycled_until_both_end(self):
        self.night()
        executor = Executor(self, ["queued/queued", "claimed/queued", "complete/complete"],
                            cycles=["busy", "complete", "complete"])
        code, out, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 0, err)
        # The two submissions, exactly, then status and cycle in turn until both ended.
        self.assertEqual(executor.calls[:2], self.submits())
        self.assertEqual(executor.operations(), ["submit", "submit", "status", "cycle",
                                                 "status", "cycle", "status"])
        for call in executor.calls[2:]:
            self.assertEqual(call[7:], [call[7]])
        # A busy cycle is waited out once, by poll_seconds.
        self.assertEqual(self.clock.slept, [60])
        self.assertEqual(out, "nightly:%s:core %s complete\nnightly:%s:telemetry %s complete\n"
                         % (TAG, CORE, TAG, TELEMETRY))
        self.assertIn("nightly: nightly:%s:core is the original %s\n" % (TAG, CORE), err)
        self.assertIn("nightly: [measure] deadline_minutes 300, poll_seconds 60 (the "
                      "defaults)\n", err)

    def test_any_terminal_state_ends_the_wait(self):
        self.night()
        for core, telemetry in (("failed", "expired"), ("uncertain", "interrupted")):
            executor = Executor(self, ["queued/queued", "%s/%s" % (core, telemetry)])
            code, out, err = self.measure(executor, "--date", "2026-10-29")
            self.assertEqual(code, 0, err)
            self.assertEqual(out, "nightly:%s:core %s %s\nnightly:%s:telemetry %s %s\n"
                             % (TAG, CORE, core, TAG, TELEMETRY, telemetry))

    def test_a_resubmitted_night_prints_the_existing_originals_and_cycles_nothing(self):
        self.night()
        executor = Executor(self, ["complete/failed"])
        code, out, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 0, err)
        self.assertEqual(executor.calls[:2], self.submits())
        self.assertEqual(executor.operations(), ["submit", "submit", "status"])
        self.assertEqual(out, "nightly:%s:core %s complete\nnightly:%s:telemetry %s failed\n"
                         % (TAG, CORE, TAG, TELEMETRY))

    def test_without_date_the_night_is_today_s_utc_date_and_said(self):
        self.night()
        executor = Executor(self, ["complete/complete"])
        code, _, err = self.measure(executor)
        self.assertEqual(code, 0, err)
        self.assertIn("nightly: --date 2026-10-29 (today's UTC date)\n", err)
        self.assertEqual(executor.calls[:2], self.submits())

    def test_a_held_journal_is_waited_for_and_any_other_submit_refusal_refuses(self):
        self.night()
        busy = ("executor_busy", "another controller holds this original journal")
        executor = Executor(self, [busy, "complete/complete"], submits=[busy])
        code, _, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 0, err)
        self.assertEqual(executor.operations(), ["submit", "submit", "submit", "status",
                                                 "status"])
        self.assertEqual(self.clock.slept, [60, 60])
        executor = Executor(self, ["complete/complete"], submits=[
            ("executor_refused", "no reachable stable release tag")])
        code, _, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 1)
        self.assertIn("nightly: refused: the nightly executor refused the nightly:%s:core "
                      "submission: no reachable stable release tag (executor_refused)" % TAG, err)
        self.assertEqual(executor.operations(), ["submit"])

    def test_a_refused_cycle_continues_only_when_an_original_moved(self):
        self.night()
        broken = ("executor_refused", "build container cleanup unconfirmed")
        executor = Executor(self, ["queued/queued", "uncertain/queued", "uncertain/complete"],
                            cycles=[broken, "complete"])
        code, out, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 0, err)
        self.assertIn("nightly:%s:core %s uncertain\n" % (TAG, CORE), out)
        executor = Executor(self, ["queued/queued"], cycles=[broken])
        code, _, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 1)
        self.assertIn("nightly: failed: the nightly executor refused to cycle and nothing "
                      "moved: build container cleanup unconfirmed (executor_refused); "
                      "nightly:%s:core is queued, nightly:%s:telemetry is queued" % (TAG, TAG),
                      err)
        self.assertEqual(executor.operations(), ["submit", "submit", "status", "cycle",
                                                 "status"])


class NightOfTheBuildUnit(Night):
    """`--night-of UNIT`: the night is the UTC date systemd last started UNIT (ADR-0273)."""

    def show(self, answer, code=0):
        asked = []

        def show(unit):
            asked.append(unit)
            return code, answer

        return show, asked

    def test_a_step_after_midnight_works_on_the_night_the_build_started(self):
        self.night()
        # The build started at 22:05:01 UTC on the 29th; the measurement starts after midnight.
        self.clock.now = 1793318400000 + 30 * 60 * 1000
        show, asked = self.show(b"@1793311501\n")
        executor = Executor(self, ["complete/complete"])
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = measure.main(["submit", "--config", self.config, "--night-of",
                                 "af-nightly-build.service"], run=executor, clock=self.clock,
                                sleep=self.clock.sleep, show=show)
        self.assertEqual(code, 0, err.getvalue())
        self.assertEqual(asked, ["af-nightly-build.service"])
        self.assertIn("nightly: --date 2026-10-29 (the UTC date af-nightly-build.service last "
                      "started)\n", err.getvalue())
        self.assertEqual(executor.calls[:2], self.submits())

    def test_a_unit_without_a_start_or_a_bad_name_is_refused(self):
        for answer, code, unit, error in (
                (b"\n", 0, "af-nightly-build.service", "systemd reports no start of it since "
                 "boot (exit 0); give the night with --date"),
                (b"@0\n", 0, "af-nightly-build.service", "no start of it since boot (@0)"),
                (b"", 1, "af-nightly-build.service", "no start of it since boot (exit 1)"),
                (b"@1793311501\n", 0, "../etc/passwd", "is not a service unit name")):
            show, asked = self.show(answer, code)
            with self.assertRaises(measure.Refused) as caught:
                measure.nightlychain.unit_night(unit, show, measure.Refused)
            self.assertIn(error, str(caught.exception))
        self.assertEqual(asked, [])

    def test_a_start_with_fractional_seconds_is_the_same_night(self):
        # systemd's unix timestamps carry the microseconds of the start.
        self.clock.now = 1793318400000 + 30 * 60 * 1000
        show, _ = self.show(b"@1793311501.659757\n")
        self.assertEqual(measure.nightlychain.unit_night("af-nightly-build.service", show,
                                                         measure.Refused, self.clock),
                         "2026-10-29")

    def test_a_build_started_more_than_a_day_ago_is_refused_as_out_of_order(self):
        self.night()
        # The build of the 29th started at 22:05:01 UTC; it is now 23 h 1 s later.
        self.clock.now = 1793311501000 + (23 * 3600 + 1) * 1000
        show, asked = self.show(b"@1793311501\n")
        executor = Executor(self, ["complete/complete"])
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = measure.main(["submit", "--config", self.config, "--night-of",
                                 "af-nightly-build.service"], run=executor, clock=self.clock,
                                sleep=self.clock.sleep, show=show)
        self.assertEqual(code, 1)
        self.assertIn("nightly: refused: --night-of af-nightly-build.service: it last started "
                      "more than 23 hours ago (@1793311501); the chain is out of order, give the "
                      "night with --date", err.getvalue())
        self.assertEqual((asked, executor.calls, out.getvalue()),
                         (["af-nightly-build.service"], [], ""))
        # A second earlier it is the build's night still.
        self.clock.now -= 2000
        self.assertEqual(measure.nightlychain.unit_night("af-nightly-build.service", show,
                                                         measure.Refused, self.clock),
                         "2026-10-29")

    def test_every_step_takes_the_night_of_a_unit_or_a_date_never_both(self):
        from tests.test_nightly_build import nightly as build
        from tests.test_nightly_publish import publish
        from tests.test_nightly_report import nightly as report
        for module, command in ((build, "build"), (build, "plan"), (measure, "submit"),
                                (report, "compose"), (publish, "publish")):
            args = module.parse([command, "--config", self.config, "--night-of",
                                 "af-nightly-build.service"])
            self.assertEqual((args.night_of, args.date), ("af-nightly-build.service", None))
            with contextlib.redirect_stderr(io.StringIO()) as err:
                with self.assertRaises(SystemExit) as caught:
                    module.parse([command, "--config", self.config, "--date", "2026-10-29",
                                  "--night-of", "af-nightly-build.service"])
            self.assertEqual(caught.exception.code, 1, command)
            self.assertIn("nightly: refused: argument --night-of: not allowed with argument "
                          "--date", err.getvalue())


class Deadline(Night):
    def test_the_deadline_fails_naming_each_original_still_open(self):
        self.night()
        executor = Executor(self, ["queued/queued"], cycles=["busy"])
        code, out, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 1)
        self.assertEqual(out, "")
        self.assertIn("nightly: failed: the deadline of 300 minutes passed: nightly:%s:core is "
                      "queued, nightly:%s:telemetry is queued\n" % (TAG, TAG), err)
        # Waits never pass the deadline, and no cycle starts after it: the last call is the
        # status that found it passed.
        self.assertEqual(sum(self.clock.slept), 300 * 60)
        self.assertTrue(all(seconds <= 60 for seconds in self.clock.slept))
        self.assertEqual(executor.operations()[-1], "status")
        self.assertEqual(self.clock.now, STARTED_MS + 300 * 60 * 1000)

    def test_a_configured_deadline_names_a_running_original(self):
        self.write_config(deadline_minutes=30, poll_seconds=600)
        self.night()
        executor = Executor(self, ["claimed/queued"], cycles=["idle"])
        code, _, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 1)
        self.assertIn("nightly: failed: the deadline of 30 minutes passed: nightly:%s:core is "
                      "running (claimed), nightly:%s:telemetry is queued\n" % (TAG, TAG), err)
        self.assertEqual(self.clock.slept, [600, 600, 600])
        self.assertNotIn("(the defaults)", err)

    def test_a_cycle_that_runs_past_the_deadline_is_waited_for(self):
        self.night()
        executor = Executor(self, ["queued/queued", "complete/complete"])

        def long_cycle(argv):
            if argv[7] == "cycle":
                self.clock.now += 400 * 60 * 1000
            return executor(argv)

        code, _, err = self.measure(long_cycle, "--date", "2026-10-29")
        self.assertEqual(code, 0, err)
        self.assertEqual(executor.operations(), ["submit", "submit", "status", "cycle",
                                                 "status"])


class SkippedOrRefused(Night):
    def test_a_skipped_night_runs_nothing(self):
        record = self.night("valid-skipped")
        executor = Executor(self, ["queued/queued"])
        code, out, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 0, err)
        self.assertEqual((executor.calls, out), ([], ""))
        self.assertIn("nightly: skipped: %s built nothing (%s); nothing to measure\n"
                      % (TAG, record["skip_reason"]), err)

    def test_a_night_without_its_record_is_refused(self):
        executor = Executor(self, ["queued/queued"])
        code, out, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 1)
        self.assertIn("nightly: refused: the night 2026-10-29 has no record: %s does not exist"
                      % os.path.join(self.builds, "2026-10-29", "nightly.json"), err)
        self.assertEqual((executor.calls, out), ([], ""))

    def test_a_configuration_without_report_is_refused(self):
        self.night()
        self.write_config(report=False)
        executor = Executor(self, ["queued/queued"])
        code, _, err = self.measure(executor, "--date", "2026-10-29")
        self.assertEqual(code, 1)
        self.assertIn("nightly: refused: %s has no [report] table" % self.config, err)
        self.assertEqual(executor.calls, [])

    def test_the_command_line_refuses_with_exit_1(self):
        executor = Executor(self, ["queued/queued"])
        for argv in (["submit"], ["submit", "--config", self.config, "--date", "2026-13-01"]):
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                try:
                    code = measure.main(argv, run=executor, clock=self.clock,
                                        sleep=self.clock.sleep)
                except SystemExit as caught:
                    code = caught.code
            self.assertEqual(code, 1, argv)
            self.assertIn("nightly: refused: ", err.getvalue())
            self.assertEqual((out.getvalue(), executor.calls), ("", []))


# The units ---------------------------------------------------------------------------------

def unit(name):
    """{section: {key: [values]}} of a unit file, comments left out."""
    sections, current = {}, None
    for line in read(os.path.join(SERVICE, name)).decode("utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            current = sections.setdefault(line.strip("[]"), {})
            continue
        key, value = line.split("=", 1)
        current.setdefault(key, []).append(value)
    return sections


class Units(TempDir):
    STEPS = {"build": ("af-nightly", ["build"]), "measure": ("af-perf-exec", ["submit"]),
             "report": ("af-perf-exec", ["compose", "--output-root /var/lib/af-nightly/reports"]),
             "publish": ("af-nightly", ["publish", "--report-root /var/lib/af-nightly/reports"])}

    def test_the_timer_starts_the_build_at_22_05_utc_and_catches_up(self):
        timer = unit("af-nightly.timer")
        self.assertEqual(timer["Timer"], {"OnCalendar": ["*-*-* 22:05:00 UTC"],
                                          "Persistent": ["true"],
                                          "Unit": ["af-nightly-build.service"]})
        self.assertEqual(timer["Install"], {"WantedBy": ["timers.target"]})

    def test_each_step_runs_its_script_from_the_installed_control_as_its_account(self):
        for step, (account, words) in self.STEPS.items():
            service = unit("af-nightly-%s.service" % step)["Service"]
            command, options = words[0], words[1:]
            # Every step works on the night the build unit started, never on a date of its
            # own: the measurement may end after midnight UTC.
            self.assertEqual(service["ExecStart"], [
                "/usr/bin/python3 -I -B /opt/af-perf/control/scripts/nightly/%s.py %s --config "
                "/etc/af-nightly/nightly.toml --night-of af-nightly-build.service%s" % (
                    step, command, "".join(" " + option for option in options))], step)
            self.assertEqual((service["Type"], service["User"], service["Group"]),
                             (["oneshot"], [account], ["af-perf-readers"]), step)
            self.assertEqual(service["Environment"], ["PYTHONDONTWRITEBYTECODE=1"], step)
            self.assertNotIn("--date", service["ExecStart"][0], "the units carry no date")
            for bound in ("TimeoutStartSec", "KillMode", "MemoryMax", "TasksMax", "LimitNOFILE",
                          "LogRateLimitIntervalSec", "LogRateLimitBurst"):
                self.assertIn(bound, service, "%s has no %s" % (step, bound))
            self.assertEqual(service["KillMode"], ["control-group"], step)

    def test_the_chain_hands_over_by_outcome(self):
        units = dict((step, unit("af-nightly-%s.service" % step)["Unit"])
                     for step in self.STEPS)
        handover = dict((step, (units[step].get("OnSuccess"), units[step].get("OnFailure")))
                        for step in self.STEPS)
        self.assertEqual(handover, {
            "build": (["af-nightly-measure.service"], None),
            "measure": (["af-nightly-report.service"], ["af-nightly-report.service"]),
            "report": (["af-nightly-publish.service"], None),
            "publish": (None, None)})
        build = unit("af-nightly-build.service")["Service"]
        self.assertEqual(build["SuccessExitStatus"], ["3"], "a skipped night is a success")
        self.assertIn("af-perf-source-refresh.service",
                      unit("af-nightly-measure.service")["Unit"]["After"][0].split())

    def test_build_and_measure_share_the_host_tmp_and_mounts(self):
        for step in ("build", "measure"):
            service = unit("af-nightly-%s.service" % step)["Service"]
            for key, value in (("PrivateTmp", "no"), ("PrivateMounts", "no"),
                               ("ProtectSystem", "no"), ("ProtectHome", "no"),
                               ("OOMPolicy", "stop")):
                self.assertEqual(service[key], [value], "%s %s" % (step, key))
        # The measure unit outlasts the longest deadline the configuration allows plus the one
        # cycle that may start before it, which the example executor bounds by run_seconds.
        timeout = unit("af-nightly-measure.service")["Service"]["TimeoutStartSec"]
        self.assertEqual(timeout, ["18h30min"])
        example = tomlfile.loads(read(os.path.join(SERVICE, "executor.example.toml")),
                                 "executor.example.toml")
        self.assertGreater(18 * 3600 + 30 * 60,
                           DEADLINE_MINUTES_MAX * 60 + example["run_seconds"])

    def test_report_and_publish_measure_nothing_and_are_hardened(self):
        for step, writes, tmp in (("report", "/var/lib/af-nightly/reports", "no"),
                                  ("publish", "/var/lib/af-nightly/builds", "yes")):
            service = unit("af-nightly-%s.service" % step)["Service"]
            self.assertEqual((service["ProtectSystem"], service["NoNewPrivileges"],
                              service["ReadWritePaths"], service["PrivateTmp"]),
                             (["strict"], ["yes"], [writes], [tmp]), step)

    def test_no_unit_carries_a_secret_value(self):
        for name in ("af-nightly.timer",) + tuple("af-nightly-%s.service" % step
                                                 for step in self.STEPS):
            text = read(os.path.join(SERVICE, name)).decode("utf-8")
            for line in text.splitlines():
                bare = line.lstrip("#")
                self.assertFalse(bare.startswith("Environment=AF_NIGHTLY_SIGN_PASSWORD"), name)
                if bare.startswith(("LoadCredential=", "EnvironmentFile=")):
                    self.assertRegex(bare.split("=", 1)[1],
                                     r"^([a-z-]+:)?/etc/af-nightly/[a-z.-]+$")
        build = read(os.path.join(SERVICE, "af-nightly-build.service")).decode("utf-8")
        self.assertIn("#LoadCredential=af-nightly-sign-password:/etc/af-nightly/sign-password",
                      build)
        self.assertIn("#EnvironmentFile=/etc/af-nightly/sign-password.env", build)
