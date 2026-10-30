"""What the steps of the nightly chain share (ADR-0272): the night a step works on.

A step is given its night by `--date YYYY-MM-DD`, or by `--night-of UNIT`: the UTC date at
which systemd last started UNIT's main process, so the four units of one chain agree on the
night the build started however long the measurement runs past midnight UTC, and a unit rerun
by hand the next morning still works on that night. Without either the night is today's UTC
date. Whichever default or unit chose the night, the step prints it.
"""
import re
import subprocess

from perflab.nightlycontracts import night_or_today, utc_date

SYSTEMCTL_SECONDS = 30
# A chain's four unit bounds add to less than a day, so the build a step follows started less
# than this many hours ago; an older start means the chain is out of order.
CHAIN_HOURS_MAX = 23
UNIT_PATTERN = r"^[A-Za-z0-9:_.@-]{1,256}\.service$"


def systemctl_show(unit):
    """`systemctl show` of UNIT's ExecMainStartTimestamp as a unix time (`@SECONDS`, or nothing
    when the unit has not started since boot): (exit code, stdout bytes)."""
    try:
        done = subprocess.run(["systemctl", "show", "--property=ExecMainStartTimestamp",
                               "--timestamp=unix", "--value", unit],
                              stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, timeout=SYSTEMCTL_SECONDS,
                              env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"})
    except (OSError, subprocess.TimeoutExpired) as error:
        return None, (str(error) or type(error).__name__).encode()
    return done.returncode, done.stdout


def unit_night(unit, show, error, clock=None):
    """The UTC date at which systemd last started `unit`, through `show`; `error` (the
    step's refusal) when the unit is not one, has not started since boot, or, given `clock`,
    started more than CHAIN_HOURS_MAX hours ago (the chain is out of order)."""
    if re.match(UNIT_PATTERN, unit) is None:
        raise error("--night-of %r is not a service unit name" % unit)
    code, out = show(unit)
    text = (out or b"").decode("ascii", "replace").strip()
    # systemd prints `@SECONDS`, with fractional seconds when the start has them.
    match = re.match(r"^@([0-9]{1,12})(?:\.[0-9]{1,9})?$", text)
    if code != 0 or match is None or int(match.group(1)) == 0:
        raise error("--night-of %s: systemd reports no start of it since boot (%s); give the "
                    "night with --date" % (unit, text or "exit %s" % code))
    started = int(match.group(1)) * 1000
    if clock is not None and clock() - started > CHAIN_HOURS_MAX * 3600 * 1000:
        raise error("--night-of %s: it last started more than %d hours ago (%s); the chain is out "
                    "of order, give the night with --date" % (unit, CHAIN_HOURS_MAX, text))
    return utc_date(started)


def add_night_options(command):
    """--date and --night-of, either one, on a step's command line."""
    night = command.add_mutually_exclusive_group()
    night.add_argument("--date", help="the night, YYYY-MM-DD (default: today's UTC date)")
    night.add_argument("--night-of", metavar="UNIT", help="the night is the UTC date at which "
                       "systemd last started UNIT (the chain's units name "
                       "af-nightly-build.service)")


def night(args, clock, show, error):
    """(date, line) of a step's night from its parsed `--date` or `--night-of`, or today's
    UTC date from `clock`; the line, when there is one, is what the step prints to say how the
    night was chosen."""
    if getattr(args, "night_of", None) is not None:
        date = unit_night(args.night_of, show, error, clock)
        return date, "nightly: --date %s (the UTC date %s last started)\n" % (date, args.night_of)
    return night_or_today(args.date, clock)
