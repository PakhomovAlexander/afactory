#!/usr/bin/python3 -IS
"""Owner-side, offline installation. Creates no credentials and contacts no service."""
import argparse
import hashlib
import importlib.util
import os
from pathlib import Path
import sys
sys.dont_write_bytecode = True


def install(config_path, output):
    source = Path(__file__).resolve().with_name("scenario-github-adapter.py")
    spec = importlib.util.spec_from_file_location("github_adapter_install", source)
    adapter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(adapter)
    config_path, output = Path(config_path), Path(output)
    raw = adapter.read_private(config_path, adapter.LIMIT)
    config = adapter.validate_config(adapter.decode(raw))
    adapter.private_path(config_path.parent, directory=True)
    adapter.outside_checkout(output)
    adapter.private_path(output.parent, directory=True)
    adapter.need(not output.exists() and not output.is_symlink(), "installation executable already exists")
    adapter.need(output != config_path and output != Path(config["token_file"]), "installation path collision")
    state = Path(config["state_dir"])
    adapter.need(not state.exists() and not state.is_symlink(), "state directory must be new; never reset receipts")
    adapter.private_path(state.parent, directory=True)
    adapter.need(state != output and state != config_path and state != Path(config["token_file"]),
                 "state path collision")
    config_digest = adapter.digest(raw)
    rendered = source.read_text().replace("CONFIG_PATH = None\n", "CONFIG_PATH = " + repr(str(config_path)) + "\n")
    rendered = rendered.replace("CONFIG_DIGEST = None\n", "CONFIG_DIGEST = " + repr(config_digest) + "\n")
    # Failures leave private evidence for the owner; reruns never erase or initialize old state.
    state.mkdir(mode=0o700)
    with (state / "lock").open("xb"):
        pass
    (state / "lock").chmod(0o600)
    adapter.atomic_write(state / "state.json", adapter.state_bytes(adapter.empty_state(config_digest)))
    with output.open("xb") as stream:
        stream.write(rendered.encode())
        stream.flush()
        os.fsync(stream.fileno())
    output.chmod(0o700)
    # Durably publish the newly created state directory as well as the executable, even
    # when their explicitly configured private parents are different directories.
    for parent in dict.fromkeys((state.parent, output.parent)):
        fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    print("sha256:" + hashlib.sha256(output.read_bytes()).hexdigest())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, help="canonical owner-only external config JSON")
    parser.add_argument("--output", required=True, help="absent executable in a private external directory")
    args = parser.parse_args()
    os.umask(0o077)
    try:
        install(args.config, args.output)
    except (OSError, ValueError, RuntimeError) as exc:
        # Refused is deliberately handled without reflecting any config or credential bytes.
        raise SystemExit("Installation refused; inspect private paths and retain any created state") from exc


if __name__ == "__main__":
    main()
