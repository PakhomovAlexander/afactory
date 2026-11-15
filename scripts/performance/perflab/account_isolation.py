"""Private OS-account identity for release measurements, outside measured child intervals.

Debug fixtures alone may carry AF_TEST_WAKE_ACCOUNT_HOME through guarded environments.
Release executables ignore that setting: local release measurements run their outer script
or supervisor in a mount/user namespace with a private files-only account database instead.
No AF production authority lookup or accounting rule is replaced.
"""
import argparse
from collections import deque
from contextlib import contextmanager, ExitStack
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


def _branches(value, cwd, entries, directories, *, required, authority):
    """Resolve components in filesystem order, retaining links and traversal directories.

    In particular, link/.. follows the link before its parent. Only canonical named
    entries become mount targets; neither dot nor parent pseudoentries can undo mounts.
    Missing optional configuration leaves retain their existing creation ancestry.
    """
    path = Path(value)
    pending = deque((path if path.is_absolute() else Path(cwd) / path).parts[1:])
    current = Path('/')
    links = 0
    while pending:
        component = pending.popleft()
        if component == '.':
            continue
        if component == '..':
            current = current.parent
            continue
        entry = current / component
        try:
            info = entry.lstat()
        except FileNotFoundError:
            if required:
                raise
            return
        entries.add(entry)
        if stat.S_ISLNK(info.st_mode):
            links += 1
            if links > 40:
                raise ValueError('too many symlinks in measurement input: %s' % value)
            target = Path(os.readlink(entry))
            if target.is_absolute():
                current = Path('/')
            pending.extendleft(reversed(target.parts[1:] if target.is_absolute() else target.parts))
        else:
            current = entry
            if stat.S_ISDIR(info.st_mode):
                # Follow Path.stat here as in the launcher's scoped foreign-owner fixture.
                if authority and entry.stat().st_uid != os.geteuid():
                    directories.add(entry)
            elif pending:
                raise NotADirectoryError(str(entry))


def namespace_directories(home, command, env, cwd, staging, resources, *,
                          inputs=(), directories=(), outputs=()):
    """Construct only explicit branches, with private containers on their real volumes.

    Required inputs are supplied by each parsed caller, independently of argv syntax.
    Fixed runtime trees are opaque; required ancestry takes precedence even inside them.
    There is no ambient sibling inventory and no fabricated filename used as a directory.
    """
    entries, containers = set(), {Path('/')}
    # Runtime inputs do not manufacture trusted ownership of executables or leaves.
    # Subtrees keep standard tools/libraries available when a required branch carves a
    # broader runtime prefix. None of these names is inferred from ambient siblings.
    runtime = ('/usr', '/bin', '/sbin', '/lib', '/lib64', '/etc', '/dev', '/proc',
               '/sys', '/run', '/var', '/usr/bin', '/usr/sbin', '/usr/lib',
               '/usr/lib64', '/usr/libexec', '/usr/share', '/usr/local',
               '/usr/local/bin', '/usr/local/lib', '/usr/local/share')
    for value in runtime:
        if Path(value).exists():
            _branches(value, cwd, entries, containers, required=True, authority=False)
    for path in os.get_exec_path(env):
        _branches(path or '.', cwd, entries, containers, required=False, authority=False)
    executable = (str(Path(cwd) / command[0]) if os.path.dirname(command[0])
                  else shutil.which(command[0], path=os.pathsep.join(
                      str(Path(cwd) / path) for path in os.get_exec_path(env))))
    if not executable:
        raise FileNotFoundError(command[0])
    _branches(executable, cwd, entries, containers, required=True,
              authority=Path(executable).stat().st_uid == os.geteuid())
    for value in (home, cwd, '/tmp', *directories):
        _branches(value, cwd, entries, containers, required=True, authority=True)
    for name in ('HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_DATA_HOME',
                 'XDG_CACHE_HOME', 'TMPDIR', 'AF_PROVIDERS_FILE', 'AF_WORKSPACE_ROOT'):
        value = env.get(name)
        if value:
            _branches(value, cwd, entries, containers, required=False, authority=True)
    for value in inputs:
        _branches(value, cwd, entries, containers, required=True, authority=True)
    for value in outputs:
        # These scripts write reports in place. A declared file must also persist when
        # its parent is reconstructed (e.g. /tmp/report.json); never bind that parent
        # back over trusted ancestry. Do not truncate existing reports during setup.
        path = Path(value)
        path = path if path.is_absolute() else Path(cwd) / path
        with path.open('ab'):
            pass
        _branches(path, cwd, entries, containers, required=True, authority=True)

    candidates = [Path(staging), Path(home), Path(cwd), Path.cwd(), Path('/tmp'), Path('/var/tmp')]
    candidates.extend(sorted(path for path in entries if path.is_dir()
                             and path.stat().st_uid == os.geteuid()))
    argv = []
    for entry in sorted(entries | containers, key=lambda path: (len(path.parts), str(path))):
        if entry in containers:
            device = entry.stat().st_dev
            # Prefer retained private storage, but never substitute its filesystem for
            # the original ancestor's. Private external containers have bounded lifetime
            # under the launch context, including failure before subprocess creation.
            base = next((path for path in candidates if path.is_dir()
                         and path.stat().st_dev == device
                         and os.access(path, os.W_OK | os.X_OK)), None)
            if base is None:
                raise ValueError('no writable backing on measurement filesystem for %s' % entry)
            backing = Path(resources.enter_context(tempfile.TemporaryDirectory(
                prefix='af-namespace-', dir=base)))
            backing.chmod(stat.S_IMODE(entry.stat().st_mode))
            argv += ['--bind', str(backing), str(entry)]
        elif entry.parent in containers:
            if entry.is_symlink():
                argv += ['--symlink', os.readlink(entry), str(entry)]
            else:
                argv += ['--dev-bind' if entry == Path('/dev') else '--bind',
                         str(entry), str(entry)]
    return argv


@contextmanager
def namespace_argv(home, command, *, env=None, cwd=None, inputs=(), directories=(), outputs=()):
    """Own a Linux launch's temporary account and same-volume directory containers.

    Preparation surrounds the outer script, outside AF's measured child intervals.
    Every caller keeps this context through process teardown; errors have no fallback.
    """
    if sys.platform == "darwin":
        yield list(command)
        return
    if sys.platform != "linux":
        raise ValueError("local release measurement account isolation currently requires Linux")
    Path(home).mkdir(mode=0o700, exist_ok=True)
    uid, gid = os.geteuid(), os.getegid()
    with ExitStack() as resources:
        passwd, nsswitch = account_files(home, uid, gid)
        resources.callback(shutil.rmtree, passwd.parent)
        argv = ["bwrap", "--unshare-user", "--uid", str(uid), "--gid", str(gid),
                "--die-with-parent", *namespace_directories(
                    home, command, os.environ if env is None else env,
                    os.getcwd() if cwd is None else cwd, passwd.parent, resources,
                    inputs=inputs, directories=directories, outputs=outputs),
                "--ro-bind", str(passwd), "/etc/passwd",
                "--ro-bind", str(nsswitch), "/etc/nsswitch.conf",
                "--setenv", "PERFLAB_ACCOUNT_FILE", str(passwd), "--", *command]
        yield argv


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


def run_script(script, args, *, inputs=(), directories=(), outputs=()):
    """Isolate a parsed measurement once without interpreting its unchanged argv."""
    if sys.platform == "darwin" or in_private_account_namespace():
        return None
    with tempfile.TemporaryDirectory(prefix="af-measurement-") as home:
        with namespace_argv(home, [sys.executable, "-I", "-B", script, *args],
                            inputs=(script, *inputs), directories=directories,
                            outputs=outputs) as argv:
            return subprocess.call(argv)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--input', action='append', default=[])
    parser.add_argument('--directory', action='append', default=[])
    parser.add_argument('--output', action='append', default=[])
    parser.add_argument('home')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        parser.error('a command is required')
    with namespace_argv(args.home, command, inputs=args.input,
                        directories=args.directory, outputs=args.output) as argv:
        return subprocess.call(argv)


if __name__ == "__main__":
    sys.exit(main())
