#!/usr/bin/env python3
"""Live Docker permission regression. Opt-in CI hook; no full AF build or measurement.

Run with python -I -B scripts/performance/tests/telemetry_build_smoke.py after pulling IMAGE.
The source is owned by the non-root runner, mode 0755, initially without target/. The exact
telemetry-contained-build command used by the workflow must compile the minimal fixture.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[3]
CLI = ROOT / "scripts/performance/cli.py"
IMAGE = ("ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:"
         "26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2")


def main():
    assert os.getuid() != 0, "smoke requires a real non-root runner-owned checkout"
    target = ROOT / "target/telemetry-build-smoke"
    target.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=target) as scratch:
        source = Path(scratch) / "source"
        source.mkdir(mode=0o755)
        source.chmod(0o755)
        (source / "src").mkdir()
        (source / "src/main.rs").write_text('fn main() { println!("contained fixture"); }\n')
        (source / "Cargo.toml").write_text(
            '[package]\nname = "af"\nversion = "0.0.0"\nedition = "2024"\n')
        (source / "Cargo.lock").write_text(
            'version = 4\n[[package]]\nname = "af"\nversion = "0.0.0"\n')
        (source / "rust-toolchain.toml").write_bytes((ROOT / "rust-toolchain.toml").read_bytes())
        (source / "build.rs").write_text('''fn main() {
    assert!(std::fs::write("/src/escaped", b"bad").is_err());
    assert!(std::fs::write("/controller-escaped", b"bad").is_err());
    for key in ["GH_TOKEN", "GITHUB_TOKEN", "ACTIONS_RUNTIME_TOKEN"] {
        assert!(std::env::var_os(key).is_none());
    }
    for key in ["CARGO_HOME", "RUSTUP_HOME", "HOME", "TMPDIR", "CARGO_TARGET_DIR"] {
        std::fs::write(std::path::Path::new(&std::env::var(key).unwrap()).join("owned"), b"ok").unwrap();
    }
}
''')
        assert source.stat().st_uid == os.getuid() and not (source / "target").exists()
        output = Path(scratch) / "build"
        subprocess.run([sys.executable, "-I", "-B", str(CLI), "telemetry-contained-build",
                        "--source", str(source), "--output", str(output), "--image", IMAGE,
                        "--seconds", "300"], check=True, timeout=420)
        binary = output / "target/release/af"
        assert binary.is_file() and binary.stat().st_uid == os.getuid()
        assert not (source / "target").exists() and not (source / "escaped").exists()
        for key in ("af-rustc", "af-cargo"):
            assert "1.88.0" in (output / key).read_text()
        env = {"PATH": "/usr/bin:/bin", "HOME": str(output / "home")}
        env.update({key: str(output / "home" / key) for key in (
            "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME", "XDG_DATA_HOME")})
        result = subprocess.run([str(binary)], env=env, capture_output=True, check=True, timeout=10)
        assert result.stdout == b"contained fixture\n"
    print("live runner-permission Docker smoke passed (minimal fixture only)")


if __name__ == "__main__":
    main()
