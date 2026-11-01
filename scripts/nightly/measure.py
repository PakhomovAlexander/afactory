#!/usr/bin/env python3
"""Submit a night's two nightly originals to the nightly executor and cycle it until both end
(ADR-0266, ADR-0273).

    python3 -I -B scripts/nightly/measure.py submit --config PATH
                                               [--date YYYY-MM-DD | --night-of UNIT]

Without --date the night is the UTC date systemd last started UNIT, or else today's UTC date,
printed on stderr (scripts/nightly/nightlychain.py). `submit` reads
builds_root/DATE/nightly.json (af.nightly-build/1). A skipped night measures nothing. For a
built night it submits the core and then the telemetry original through the executor of the
tree it runs from (`scripts/performance/cli.py executor --config <[report] executor_config>
submit --work core|telemetry --kind nightly --tag TAG --head HEAD_SHA`; a resubmission prints
the existing original), then runs that executor's `status` and `cycle` in turn until `status`
shows both originals terminal or `[measure] deadline_minutes` pass. After a `busy` or `idle`
cycle, or while another process holds the journal, it waits `[measure] poll_seconds`, never
past the deadline. No cycle starts after the deadline, and a cycle that runs is never
interrupted: the executor bounds it by its own run_seconds. The nightly executor's journal is
submitted to and cycled by this step alone; it never reads or writes the journal itself.

Exit codes: 0 both originals ended (whatever their outcome) or the night was skipped; 1 failed
(`nightly: failed: ...`, naming each original still open when the deadline passed or the
executor refused a status or a cycle that moved nothing), or refused (`nightly: refused: ...`:
the night has no nightly.json, the configuration has no [report] table or is malformed, the
command line is malformed, or the executor refused a submission). docs/nightly.md is the
operator page.
"""
import argparse
import json
import os
import subprocess
import sys
import time

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(ROOT, "scripts", "performance"))
sys.path.insert(0, HERE)

import nightlychain  # noqa: E402
from perflab import contracts, tomlfile  # noqa: E402
from perflab.nightlycontracts import DEADLINE_MINUTES_DEFAULT, POLL_SECONDS_DEFAULT  # noqa: E402
from perflab.values import ContractError  # noqa: E402

EXIT_MEASURED, EXIT_FAILED = 0, 1
# The executor of the tree this script runs from: the installed control, never another
# checkout.
CLI = os.path.join(ROOT, "scripts", "performance", "cli.py")
# The originals of a night, in the order they are submitted (ADR-0266).
WORKS = ("core", "telemetry")
# The states after which the executor never runs an original again.
TERMINAL = ("complete", "failed", "interrupted", "uncertain", "expired")
# What the executor's refusal names when another process holds the journal: nothing was
# submitted or launched, so the call is made again after a wait.
JOURNAL_BUSY = "executor_busy"
WAITING = ("busy", "idle")


class Refused(Exception):
    """The night cannot be measured; nothing was submitted, or a submission was refused."""


class Failed(Exception):
    """The night's originals were submitted but did not both end; the message names them."""


def now_ms():
    return int(time.time() * 1000)


def run_command(argv):
    """One executor call: its stdout, its stderr passed through to this step's journal. The
    executor bounds each call itself (cycle_seconds, run_seconds); the unit's TimeoutStartSec
    is the backstop."""
    try:
        return subprocess.run(argv, stdout=subprocess.PIPE)
    except OSError as error:
        raise Refused("%s: %s" % (argv[0], error))


# Configuration and the night ----------------------------------------------------------------

def load_config(path):
    """The af.nightly-config/1 TOML at `path`, its `[report]` table required (its
    executor_config is the executor measured on) and `[measure]` defaulted."""
    try:
        with open(path, "rb") as stream:
            data = stream.read(tomlfile.MAX_TOML_BYTES + 1)
    except OSError as error:
        raise Refused("--config %s: %s" % (path, error))
    doc = contracts.validate_nightly_config(tomlfile.loads(data, path), path)
    if "report" not in doc:
        raise Refused("%s has no [report] table: measuring needs its executor_config "
                      "(docs/nightly.md)" % path)
    table = dict(doc.get("measure", {}))
    defaults = []
    for key, value in (("deadline_minutes", DEADLINE_MINUTES_DEFAULT),
                       ("poll_seconds", POLL_SECONDS_DEFAULT)):
        if key not in table:
            table[key] = value
            defaults.append("%s %d" % (key, value))
    return dict(doc, measure=table), defaults


def read_night(builds_root, date):
    path = os.path.join(builds_root, date, "nightly.json")
    if not os.path.isfile(path):
        raise Refused("the night %s has no record: %s does not exist" % (date, path))
    try:
        record = contracts.read_file(path, "nightly-build")
    except (OSError, ContractError) as error:
        raise Refused("%s: %s" % (path, error))
    if record["date"] != date:
        raise Refused("%s records the night %s" % (path, record["date"]))
    return record


# The executor -------------------------------------------------------------------------------

class Executor:
    """The nightly executor's `submit`, `status` and `cycle`, each one process of `CLI` run
    through `run` (the tests' fake)."""

    def __init__(self, config_path, run):
        self.config_path = config_path
        self.run = run

    def call(self, operation, *options):
        """(None, stdout text) of a call that succeeded, else (the refusal's reason, its
        detail)."""
        argv = [sys.executable, "-I", "-B", CLI, "executor", "--config", self.config_path,
                operation] + list(options)
        result = self.run(argv)
        out = (result.stdout or b"").decode("utf-8", "replace")
        if result.returncode == 0:
            return None, out
        try:
            refusal = json.loads(out)
            reason, detail = refusal["reason"], refusal["detail"]
        except (ValueError, TypeError, KeyError):
            reason, detail = "exit %d" % result.returncode, out.strip()[-300:] or "no detail"
        return str(reason), str(detail)

    def submit(self, work, tag, head):
        """(request_id, key, None) of the original, a resubmission of the night giving the
        existing one, or (None, key, (reason, detail)) of a refusal."""
        key = "nightly:%s:%s" % (tag, work)
        reason, out = self.call("submit", "--work", work, "--kind", "nightly", "--tag", tag,
                                "--head", head)
        if reason is not None:
            return None, key, (reason, out)
        try:
            request = json.loads(out)
            ident, submitted = request["request_id"], request["submission"]["key"]
        except (ValueError, TypeError, KeyError):
            raise Refused("the executor answered the %s submission with no request" % key)
        if submitted != key or not isinstance(ident, str):
            raise Refused("the executor answered the %s submission with the original %r of %r"
                          % (key, ident, submitted))
        return ident, key, None

    def status(self, idents):
        """{request_id: state} of `idents`, or (reason, detail) of a refusal."""
        reason, out = self.call("status")
        if reason is not None:
            return None, (reason, out)
        try:
            states = dict((item["request_id"], item["state"]) for item in json.loads(out))
        except (ValueError, TypeError, KeyError):
            raise Failed("the executor's status is no list of original states")
        missing = [ident for ident in idents if ident not in states]
        if missing:
            raise Failed("the executor's status does not list %s" % ", ".join(missing))
        return dict((ident, states[ident]) for ident in idents), None

    def cycle(self):
        """(outcome, None) of a cycle, or (None, (reason, detail)) of a refusal."""
        reason, out = self.call("cycle")
        if reason is not None:
            return None, (reason, out)
        return out.strip(), None


def shown(state):
    return "running (claimed)" if state == "claimed" else state


def measure(config, record, run, clock, sleep):
    """{key: (request_id, state)} once both originals are terminal; Failed when the deadline
    passes first or the executor refuses what it should answer."""
    started = clock()
    deadline = started + config["measure"]["deadline_minutes"] * 60 * 1000
    poll = config["measure"]["poll_seconds"]
    executor = Executor(config["report"]["executor_config"], run)

    def wait(why):
        seconds = min(poll, (deadline - clock()) / 1000.0)
        if seconds > 0:
            sys.stderr.write("nightly: %s; waiting %d s\n" % (why, max(1, int(seconds))))
            sleep(seconds)

    keys = {}
    for work in WORKS:
        while True:
            ident, key, refusal = executor.submit(work, record["tag"], record["head_sha"])
            if ident is not None:
                break
            if refusal[0] != JOURNAL_BUSY or clock() >= deadline:
                raise Refused("the nightly executor refused the %s submission: %s (%s)"
                              % (key, refusal[1], refusal[0]))
            wait("another process holds the nightly executor's journal")
        if ident in keys:
            raise Refused("the nightly executor returned the original %s for both %s and %s"
                          % (ident, keys[ident], key))
        keys[ident] = key
        sys.stderr.write("nightly: %s is the original %s\n" % (key, ident))

    def still_open(states):
        return ", ".join("%s is %s" % (keys[ident], shown(state))
                         for ident, state in states.items() if state not in TERMINAL)

    previous, refused = None, None
    while True:
        states, refusal = executor.status(list(keys))
        if refusal is not None:
            if refusal[0] != JOURNAL_BUSY:
                raise Failed("the nightly executor refused its status: %s (%s)"
                             % (refusal[1], refusal[0]))
            if clock() >= deadline:
                raise Failed("the deadline of %d minutes passed while another process held "
                             "the nightly executor's journal; %s"
                             % (config["measure"]["deadline_minutes"],
                                ", ".join(sorted(keys.values()))
                                + " may still be open"))
            wait("another process holds the nightly executor's journal")
            continue
        if all(state in TERMINAL for state in states.values()):
            return dict((keys[ident], (ident, state)) for ident, state in states.items())
        if refused is not None and states == previous:
            raise Failed("the nightly executor refused to cycle and nothing moved: %s (%s); "
                         "%s" % (refused[1], refused[0], still_open(states)))
        if clock() >= deadline:
            raise Failed("the deadline of %d minutes passed: %s"
                         % (config["measure"]["deadline_minutes"], still_open(states)))
        previous, refused = states, None
        outcome, refusal = executor.cycle()
        if refusal is not None:
            if refusal[0] == JOURNAL_BUSY:
                wait("another process holds the nightly executor's journal")
            else:
                # A cycle that ended an original badly refuses too; the next status says
                # whether anything moved.
                refused = refusal
        elif outcome in WAITING:
            wait("the nightly executor's cycle is %s" % outcome)
        else:
            sys.stderr.write("nightly: a cycle ended an original: %s\n" % outcome)


# Command line -------------------------------------------------------------------------------

class Parser(argparse.ArgumentParser):
    def error(self, message):
        self.print_usage(sys.stderr)
        sys.stderr.write("nightly: refused: %s\n" % message)
        sys.exit(EXIT_FAILED)


def parse(argv):
    parser = Parser(prog="measure.py", description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", parser_class=Parser)
    commands.required = True
    command = commands.add_parser("submit", help="submit the night's core and telemetry "
                                  "originals and cycle the nightly executor until both end")
    command.add_argument("--config", required=True, help="the af.nightly-config/1 TOML with "
                         "its [report] table")
    nightlychain.add_night_options(command)
    return parser.parse_args(argv)


def main(argv=None, run=run_command, clock=now_ms, sleep=time.sleep,
         show=nightlychain.systemctl_show):
    args = parse(argv)
    try:
        date, chosen = nightlychain.night(args, clock, show, Refused)
        if chosen:
            sys.stderr.write(chosen)
        config, defaults = load_config(args.config)
        if defaults:
            sys.stderr.write("nightly: [measure] %s (the defaults)\n" % ", ".join(defaults))
        record = read_night(config["builds_root"], date)
        if record["status"] == "skipped":
            sys.stderr.write("nightly: skipped: %s built nothing (%s); nothing to measure\n"
                             % (record["tag"], record["skip_reason"]))
            return EXIT_MEASURED
        ended = measure(config, record, run, clock, sleep)
    except (Refused, ContractError) as error:
        sys.stderr.write("nightly: refused: %s\n" % error)
        return EXIT_FAILED
    except Failed as error:
        sys.stderr.write("nightly: failed: %s\n" % error)
        return EXIT_FAILED
    for key in sorted(ended):
        sys.stdout.write("%s %s %s\n" % (key, ended[key][0], ended[key][1]))
    return EXIT_MEASURED


if __name__ == "__main__":
    sys.exit(main())
