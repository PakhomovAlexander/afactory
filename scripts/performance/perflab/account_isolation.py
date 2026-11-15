"""Private OS-account identity for release measurements, outside measured child intervals.

Debug fixtures alone may carry AF_TEST_WAKE_ACCOUNT_HOME through guarded environments.
Release executables ignore that setting: local release measurements run their outer script
or supervisor in a mount/user namespace with a private files-only account database instead.
No AF production authority lookup or accounting rule is replaced.
"""
import os
import pwd
import shutil
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


def namespace_directories(home, command, env, cwd, staging):
    """Private mount points for ancestors which the single-UID map cannot represent.

    Bubblewrap maps only the caller, so host root becomes the overflow UID for a
    non-root caller. Recreate directory *containers*, never chown host paths or map
    overflow ownership to a trusted UID. Bind only required branches at the same
    names: files and private HOME/Store/source subtrees retain their backing inodes.
    Foreign leaves remain foreign. Original permission bits, including sticky bits,
    are retained; this does not turn an unsafe writable directory into a safe one.
    """
    # Bubblewrap itself needs /tmp, including nested launches with every input
    # under /home and an explicit TMPDIR elsewhere. Retain its mode and volume.
    paths = [Path(home), Path(cwd) / 'entry', Path('/tmp') / 'entry']
    for name in ('HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_DATA_HOME',
                 'XDG_CACHE_HOME', 'TMPDIR', 'AF_PROVIDERS_FILE', 'AF_WORKSPACE_ROOT'):
        value = env.get(name)
        if value and Path(value).is_absolute():
            paths.append(Path(value))
    exec_path = [Path(cwd) / path for path in os.get_exec_path(env)]
    paths.extend(path / 'entry' for path in exec_path)
    paths.extend(Path(arg) for arg in command if Path(arg).is_absolute())
    name = str(Path(cwd) / command[0]) if os.path.dirname(command[0]) else command[0]
    executable = shutil.which(name, path=os.pathsep.join(map(str, exec_path)))
    if executable:
        paths.append(Path(executable))
    # Runtime trees are opaque inputs, not inventories of ambient /tmp or /home.
    # Preserve the loader, libraries, NSS, devices and subprocess tools together.
    runtime = {Path(path) for path in (
        '/usr', '/bin', '/sbin', '/lib', '/lib64', '/etc', '/dev', '/proc', '/sys',
        '/run', '/var') if Path(path).exists()}
    paths.extend(runtime)
    directories = {Path('/')}
    entries = set()
    seen = set()
    uid = os.geteuid()
    while paths:
        path = paths.pop()
        if path in seen:
            continue
        seen.add(path)
        resolved = path.resolve()
        for parent in resolved.parents:
            # Runtime trees retain their original leaves and ownership, including
            # foreign executable leaves. Never manufacture trusted executable owners.
            if any(parent == root or root in parent.parents for root in runtime):
                continue
            if parent.is_dir() and parent.stat().st_uid != uid:
                directories.add(parent)
        for entry in (*reversed(path.parents), path):
            if entry == Path('/'):
                continue
            # Retain lexical symlinks as well as their canonical target branches.
            # Checking prefixes also captures intermediate links in a link chain.
            canonical = entry.parent.resolve() / entry.name
            if canonical.is_symlink():
                paths.append(canonical.parent / os.readlink(canonical))
            if canonical.exists() or canonical.is_symlink():
                entries.add(canonical)
    argv = []
    for directory in sorted(directories, key=lambda path: (len(path.parts), str(path))):
        mode = stat.S_IMODE(directory.stat().st_mode)
        # Use the private account's existing filesystem for the mount points. A new
        # tmpfs would replace disk capacity with the memory limit and change AF's
        # free-space admission and measurement behavior.
        backing = Path(tempfile.mkdtemp(prefix='directory-', dir=staging))
        backing.chmod(mode)
        argv += ['--bind', str(backing), str(directory)]
        for entry in sorted(entry for entry in entries
                            if entry.parent == directory and entry not in directories):
            if entry.is_symlink():
                argv += ['--symlink', os.readlink(entry), str(entry)]
            else:
                argv += ['--dev-bind' if entry == Path('/dev') else '--bind',
                         str(entry), str(entry)]
    return argv


def namespace_argv(home, command, *, env=None, cwd=None):
    """Prepare account files and a Linux launcher; failure has no unisolated fallback.

    Run before the script/supervisor starts its AF timers, so wait4 still measures the
    actual AF child. Private ancestor mounts and account files leave source/binary
    bytes, paths, supervision, limits and counters unchanged.
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
            "--die-with-parent", *namespace_directories(
                home, command, os.environ if env is None else env,
                os.getcwd() if cwd is None else cwd, passwd.parent),
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
