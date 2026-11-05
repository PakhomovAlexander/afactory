#!/usr/bin/python3 -IS
"""Install the narrow implementation lifecycle adapter offline; contact no service."""
import argparse
import importlib.util
import os
from pathlib import Path
import sys
sys.dont_write_bytecode = True


def source():
    root = Path(__file__).resolve().parent
    common = (root / "scenario-github-adapter.py").read_text().split('if __name__ == "__main__":')[0]
    extension = (root / "scenario-impl-adapter.py").read_text()
    return common + "\noriginal_validate_config = validate_config\n" + extension


def module():
    value = importlib.util.module_from_spec(importlib.util.spec_from_loader("impl_install", loader=None))
    exec(compile(source().split('if __name__ == "__main__":')[0], "impl_adapter", "exec"), value.__dict__)
    return value


def install(config_path, output):
    a = module()
    config_path, output = Path(config_path), Path(output)
    raw = a.read_private(config_path, a.LIMIT)
    c = a.validate_config(a.decode(raw))
    a.private_path(config_path.parent, directory=True)
    a.outside_checkout(output)
    a.private_path(output.parent, directory=True)
    state = Path(c["state_dir"])
    a.private_path(state.parent, directory=True)
    a.private_path(Path(c["worktree_root"]), directory=True)
    for path in (state, output):
        a.need(not path.exists() and not path.is_symlink(), "installation/state must be absent; never reset receipts")
    a.need(len({str(config_path), str(output), c["token_file"], c["ssh_config"], str(state)}) == 5,
           "installation path collision")
    for key in ("af", "git"):
        path = Path(c[key])
        a.need(path.is_file() and a.file_digest(path) == c[key + "_digest"], "installed tool digest differs")
    a.need(a.digest(a.read_private(Path(c["ssh_config"]), a.LIMIT)) == c["ssh_config_digest"], "SSH config digest differs")
    state.mkdir(mode=0o700)
    (state / "home").mkdir(mode=0o700)
    with (state / "lock").open("xb"):
        pass
    (state / "lock").chmod(0o600)
    config_digest = a.digest(raw)
    a.repository_settings(c)
    a.atomic_write(state / "state.json", a.state_bytes(dict(schema=a.IMPL_STATE, config_digest=config_digest, records={})))
    rendered = source().replace("CONFIG_PATH = None\n", "CONFIG_PATH = " + repr(str(config_path)) + "\n")
    rendered = rendered.replace("CONFIG_DIGEST = None\n", "CONFIG_DIGEST = " + repr(config_digest) + "\n")
    with output.open("xb") as stream:
        stream.write(rendered.encode())
        stream.flush()
        os.fsync(stream.fileno())
    output.chmod(0o700)
    for parent in dict.fromkeys((state.parent, output.parent)):
        fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    print(a.digest(output.read_bytes()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        install(args.config, args.output)
    except (OSError, ValueError, RuntimeError):
        raise SystemExit("Implementation installation refused; retain any created state and inspect private paths")


if __name__ == "__main__":
    main()
