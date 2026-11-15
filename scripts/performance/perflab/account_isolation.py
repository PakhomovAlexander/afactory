"""Private OS-account identity for release measurements, outside measured child intervals.

Debug fixtures alone may carry AF_TEST_WAKE_ACCOUNT_HOME through guarded environments.
Release executables ignore that setting: local release measurements run their outer script
or supervisor in a mount/user namespace with a private files-only account database instead.
No AF production authority lookup or accounting rule is replaced.
"""
import os
import pwd
import subprocess
from pathlib import Path
import stat
import sys
import tempfile

FIXTURE = "AF_TEST_WAKE_ACCOUNT_HOME"


def private_home(home):
    path = Path(home)
    if not path.is_absolute() or path == Path("/") or path.resolve() != path:
        raise ValueError("measurement account home must be canonical, absolute and non-root")
    if any(c in str(path) for c in (":", "\n", "\r")):
        raise ValueError("measurement account home cannot contain account-file delimiters")
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise ValueError("measurement account home must be owner-private")
    return path


def fixture_environment(env, parent=None):
    """Retain only the exact fixture marker of the validated private invoking HOME.

    No other AF setting is inherited. Absence is not permission to redirect a release's
    account: those executions must use namespace_argv around the outer measurement.
    """
    parent = os.environ if parent is None else parent
    if FIXTURE in parent:
        home = private_home(parent[FIXTURE])
        if str(home) != parent.get("HOME"):
            raise ValueError("fixture account marker must name the private invoking HOME")
        env[FIXTURE] = str(home)
    return env


def account_files(home, uid, gid):
    """Fixed private NSS inputs, also mounted by the existing contained executor."""
    home = private_home(home)
    directory = Path(tempfile.mkdtemp(prefix="account-", dir=home))
    passwd = directory / "passwd"
    passwd.write_text("af-measurement:x:%d:%d:AF measurement:%s:/bin/false\n" % (uid, gid, home))
    nsswitch = directory / "nsswitch.conf"
    nsswitch.write_text("passwd: files\ngroup: files\nhosts: files dns\n")
    return passwd, nsswitch


def namespace_argv(home, command):
    """Prepare account files and a Linux launcher; failure has no unisolated fallback.

    Run before the script/supervisor starts its AF timers, so wait4 still measures the
    actual AF child. Only the account database is overlaid; source/binary paths and
    existing process supervision, limits and counters retain their meanings.
    """
    # AF has no OS-account wake authority on macOS, so there is nothing to overlay.
    if sys.platform == "darwin":
        return list(command)
    if sys.platform != "linux":
        raise ValueError("local release measurement account isolation currently requires Linux")
    # Some untimed CLI fixture setups have not made HOME yet. Their private scratch
    # parent already exists; do not adopt or chmod any existing directory here.
    Path(home).mkdir(mode=0o700, exist_ok=True)
    uid, gid = os.geteuid(), os.getegid()
    passwd, nsswitch = account_files(home, uid, gid)
    return ["bwrap", "--unshare-user", "--uid", str(uid), "--gid", str(gid),
            "--die-with-parent", "--bind", "/", "/", "--dev-bind", "/dev", "/dev",
            "--ro-bind", str(passwd), "/etc/passwd",
            "--ro-bind", str(nsswitch), "/etc/nsswitch.conf",
            "--setenv", "PERFLAB_ACCOUNT_FILE", str(passwd), "--", *command]


def in_private_account_namespace():
    """Check the actual mounted account file and NSS result, never just an env claim."""
    path = os.environ.get("PERFLAB_ACCOUNT_FILE")
    if not path:
        return False
    account = pwd.getpwuid(os.geteuid())
    home = private_home(account.pw_dir)
    path = Path(path)
    if (account.pw_name != "af-measurement" or path.parent.parent != home
            or path.parent.parent.resolve() != home):
        raise ValueError("invalid private measurement account binding")
    if not os.path.samestat(path.stat(), Path("/etc/passwd").stat()):
        raise ValueError("measurement account file is not the active OS account database")
    return True


def run_script(script, args):
    """Isolate a standalone measurement once, regardless of any debug fixture override."""
    if sys.platform == "darwin" or in_private_account_namespace():
        return None
    with tempfile.TemporaryDirectory(prefix="af-measurement-") as home:
        return subprocess.call(namespace_argv(home, [sys.executable, "-I", "-B", script, *args]))


def main():
    # An outer launcher for standalone release scripts (no AF fixture override).
    if len(sys.argv) < 3:
        raise ValueError("usage: account_isolation.py PRIVATE_HOME COMMAND...")
    return subprocess.call(namespace_argv(sys.argv[1], sys.argv[2:]))


if __name__ == "__main__":
    sys.exit(main())
