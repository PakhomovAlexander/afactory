#!/usr/bin/python3 -IS
"""Offline owner installation of the Git workspace capability and production GitHub adapter."""
import argparse
from pathlib import Path
import os
import sys
sys.dont_write_bytecode = True


def source():
    root = Path(__file__).resolve().parent
    common = (root / "scenario-github-adapter.py").read_text().split('if __name__ == "__main__":')[0]
    workspace = (root / "scenario-git-workspace.py").read_text()
    return common + "\n" + workspace


def module():
    import types
    result = types.ModuleType("installed_workspace")
    exec(compile(source(), "<pinned workspace adapter>", "exec"), result.__dict__)
    return result


def install(config_path, output):
    a = module()
    config_path, output = Path(config_path), Path(output)
    raw = a.read_private(config_path, a.LIMIT)
    c = a.workspace_validate_config(a.decode(raw))
    a.private_path(config_path.parent, directory=True)
    a.outside_checkout(output)
    a.private_path(output.parent, directory=True)
    a.need(not output.exists() and not output.is_symlink(), "output must be absent")
    a.need(output not in [config_path, Path(c["token_file"]), Path(c["ssh_config"]), Path(c["af"]), Path(c["git"])], "path collision")
    state = Path(c["state_dir"])
    a.need(not state.exists() and not state.is_symlink(), "state must be new; retain old receipts")
    a.private_path(state.parent, directory=True)
    a.private_path(Path(c["worktree_root"]), directory=True)
    for key in ("af", "git"):
        a.need(a.file_digest(c[key]) == c[key + "_digest"], "tool digest differs")
    a.need(a.digest(a.read_private(Path(c["ssh_config"]), a.LIMIT)) == c["ssh_config_digest"], "SSH config differs")
    pin = a.digest(raw)
    rendered = source().replace("CONFIG_PATH = None\n", "CONFIG_PATH = " + repr(str(config_path)) + "\n")
    rendered = rendered.replace("CONFIG_DIGEST = None\n", "CONFIG_DIGEST = " + repr(pin) + "\n")
    state.mkdir(mode=0o700)
    (state / "home").mkdir(mode=0o700)
    with (state / "lock").open("xb") as stream:
        stream.flush()
        os.fsync(stream.fileno())
    (state / "lock").chmod(0o600)
    a.atomic_write(state / "state.json", a.state_bytes(dict(schema=a.WORKSPACE_STATE, config_digest=pin, records={})))
    a.atomic_write(state / "github.json", a.state_bytes(a.empty_state(pin)))
    a.repository_settings(c)
    rendered += '''\nif __name__ == "__main__":
    try:
        workspace_main()
    except Refused as exc:
        sys.stderr.write("Workspace adapter refused: " + str(exc) + "; retain private receipts and owned objects\\n")
        sys.exit(1)
    except (OSError, ValueError, TypeError, KeyError, RecursionError, subprocess.SubprocessError):
        sys.stderr.write("Workspace adapter refused; retain private receipts and owned objects\\n")
        sys.exit(1)
'''
    with output.open("xb") as stream:
        stream.write(rendered.encode())
        stream.flush()
        os.fsync(stream.fileno())
    output.chmod(0o700)
    for parent in dict.fromkeys((state, state.parent, output.parent)):
        a.sync_directory(parent)
    print(a.digest(output.read_bytes()))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        install(args.config, args.output)
    except (OSError, ValueError, RuntimeError) as exc:
        raise SystemExit("Installation refused; retain private state") from exc
