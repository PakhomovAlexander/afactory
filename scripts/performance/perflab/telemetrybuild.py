"""Hosted telemetry build using the lab's verified image and direct toolchain machinery.

Tool probes have no candidate mount. Compilation has a read-only source mount and only an
owned state directory writable; neither the Docker socket nor runner credentials are mounted.
"""
import os
import secrets
import subprocess

from . import builds, tomlfile
from .values import require


def run(source, output, reference, seconds=4800):
    require(1 <= seconds <= 4800, "build deadline must be within 1..4800 seconds")
    source, output = os.path.abspath(source), os.path.abspath(output)
    require(not os.path.lexists(output), "build output must be a new owned directory")
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(__file__))))
    with open(os.path.join(root, "rust-toolchain.toml"), "rb") as stream:
        trusted = tomlfile.loads(stream.read(), "trusted toolchain")["toolchain"]["channel"]
    with open(os.path.join(source, "rust-toolchain.toml"), "rb") as stream:
        channel = tomlfile.loads(stream.read(), "source toolchain")["toolchain"]["channel"]
    require(channel == trusted, "source toolchain differs from trusted control")
    image = builds.BuildImage("docker", reference)
    tool = builds.ContainerToolchain(image, trusted)
    os.mkdir(output, 0o700)
    for name in ("home", "tmp", "cargo-home", "rustup-home", "target"):
        os.mkdir(os.path.join(output, name), 0o700)
    env = {"PATH": tool.bin_dir + ":/usr/bin:/bin", "LC_ALL": "C",
           "HOME": "/state/home", "TMPDIR": "/state/tmp", "CARGO_HOME": "/state/cargo-home",
           "RUSTUP_HOME": "/state/rustup-home", "CARGO_TARGET_DIR": "/state/target",
           "RUSTC": tool.rustc, "RUSTDOC": os.path.join(tool.bin_dir, "rustdoc")}
    name = "af-telemetry-build-" + secrets.token_hex(6)
    command = image.command(name, [tool.cargo, "build", "--release", "--locked", "--bin", "af"],
                            "/src", env, [(source, "/src", False), (output, "/state", True)],
                            network=True)
    command[2:2] = ["--memory", "8g", "--memory-swap", "8g", "--pids-limit", "512",
                    "--ulimit", "fsize=8589934592:8589934592"]
    try:
        subprocess.run(command, check=True, stdin=subprocess.DEVNULL,
                       env=builds.docker_client_environment(), timeout=seconds)
    finally:
        problem = image.remove(name)
        require(problem is None, problem or "")
    for filename, identity in (("af-rustc", tool.rustc_identity), ("af-cargo", tool.cargo_identity)):
        with open(os.path.join(output, filename), "w") as stream:
            stream.write(identity + "\n")
