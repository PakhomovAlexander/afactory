#!/usr/bin/env python3
"""Token-free Scenario lab adapter, native submission service and bounded evidence retrieval.

This is an operator entry point. No subcommand installs a service or enables an Ant. The
generated fixed-argument launcher is the program machine Scenario policy pins by digest.
"""
import argparse
import hashlib
import json
import os
import pathlib
import sys

sys.dont_write_bytecode = True
HERE = pathlib.Path(__file__).resolve().parent
if "perflab" not in sys.modules:
    sys.path.insert(0, str(HERE))

from perflab import contracts, executor, researchevidence, scenarioadapter  # noqa: E402
from perflab.values import ContractError, canonical_digest, document_bytes, loads_strict, require  # noqa: E402


def parser():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="command", required=True)
    for name, help_text in (("adapter", "one bounded stdin/stdout adapter call"),
                            ("submit", "submit one durable intent through native preflight"),
                            ("verify", "verify native results offline; launch no compute"),
                            ("launcher", "write an absent fixed-argument, closure-pinned launcher")):
        cmd = sub.add_parser(name, help=help_text)
        cmd.add_argument("--config", required=True)
        if name == "launcher":
            cmd.add_argument("--output", required=True)
    cmd = sub.add_parser("evidence", help="rebuild an index from explicit native source selections")
    cmd.add_argument("--sources", required=True, help="JSON array of [path, raw digest, timestamp, compatibility]")
    cmd.add_argument("--executor", help="native executor config, with explicit --original IDs")
    cmd.add_argument("--original", action="append", default=[])
    cmd.add_argument("--output-root", required=True, help="existing private content-addressed evidence root")
    cmd.add_argument("--compatibility", required=True)
    cmd.add_argument("--cutoff", type=int, required=True)
    cmd.add_argument("--created", type=int, required=True)
    cmd.add_argument("--max-age-ms", type=int, required=True)
    cmd.add_argument("--max-items", type=int, default=32)
    cmd.add_argument("--max-bytes", type=int, default=24576)
    return p


def launcher(config_path, output):
    config = pathlib.Path(config_path).resolve()
    scenarioadapter.load_config(str(config))
    # Bind every executable Python dependency before any of them is imported. Changing a
    # module or configuration requires a new launcher and new machine-policy executable pin.
    files = [pathlib.Path(__file__).resolve(), config] + sorted((HERE / "perflab").rglob("*.py"))
    digests = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
    modules = {".".join(p.relative_to(HERE).with_suffix("").parts).removesuffix(".__init__"):
               (str(p), p.read_text()) for p in sorted((HERE / "perflab").rglob("*.py"))}
    # Execute only the pinned sources, never adjacent shadow modules or cached bytecode.
    frozen = ("import importlib.abc, importlib.util\n"
        "_sources = " + repr(modules) + "\n"
        "class _Pinned(importlib.abc.MetaPathFinder, importlib.abc.Loader):\n"
        "    def find_spec(self, name, path=None, target=None):\n"
        "        if name == 'perflab' or name.startswith('perflab.'):\n"
        "            if name not in _sources: raise ImportError('unpinned adapter module: ' + name)\n"
        "            return importlib.util.spec_from_loader(name, self, is_package=_sources[name][0].endswith('/__init__.py'))\n"
        "    def create_module(self, spec): return None\n"
        "    def exec_module(self, module):\n"
        "        path, text = _sources[module.__name__]\n"
        "        module.__file__ = path\n"
        "        exec(compile(text, path, 'exec'), module.__dict__)\n"
        "sys.meta_path.insert(0, _Pinned())\nimport perflab\n")
    source = ("#!/usr/bin/python3 -I\nimport sys\nsys.dont_write_bytecode = True\n"
        "import hashlib, os, stat\n"
        "for path, expected in " + repr(digests) + ".items():\n"
        "    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)\n"
        "    with os.fdopen(fd, 'rb') as held:\n"
        "        info = os.fstat(held.fileno())\n"
        "        if not stat.S_ISREG(info.st_mode) or info.st_mode & 0o022 or info.st_size > 8388608:\n"
        "            raise SystemExit('unsafe installed adapter closure')\n"
        "        if hashlib.sha256(held.read(8388609)).hexdigest() != expected:\n"
        "            raise SystemExit('installed adapter closure changed')\n"
        + frozen + "sys.argv = [" + repr(str(pathlib.Path(__file__).resolve())) + ", 'adapter', '--config', "
        + repr(str(config)) + "]\nexec(compile(" + repr(pathlib.Path(__file__).read_text())
        + ", sys.argv[0], 'exec'), {'__name__': '__main__', '__file__': sys.argv[0]})\n")
    with open(output, "x") as stream:
        stream.write(source)
        stream.flush()
        os.fsync(stream.fileno())
    os.chmod(output, 0o755)
    executor.sync_dir(os.path.dirname(os.path.abspath(output)))
    return {"schema": "af.perf-scenario-launcher/1", "executable": os.path.abspath(output),
            "digest": "sha256:" + hashlib.sha256(source.encode()).hexdigest()}


def main(argv=None):
    args = parser().parse_args(argv)
    if args.command == "evidence":
        sources = loads_strict(executor.read(args.sources), "selected native sources", 1048576)
        records = researchevidence.captured(sources)
        require(not args.original or args.executor, "native original selection needs --executor")
        if args.original:
            with executor.Journal(executor.load_config(args.executor), readonly=True) as journal:
                records += researchevidence.native(journal, args.original)
        index = researchevidence.rebuild(records)
        packet = researchevidence.packet(index, args.cutoff, args.created, args.compatibility,
            args.max_age_ms, args.max_items, args.max_bytes)
        result = {"schema": "af.perf-evidence-export/1",
                  "index": researchevidence.retain(args.output_root, index, "evidence-index"),
                  "index_digest": canonical_digest(index),
                  "packet": researchevidence.retain(args.output_root, packet, "evidence-packet")}
    elif args.command == "launcher":
        result = launcher(args.config, args.output)
    else:
        adapter = scenarioadapter.Adapter(scenarioadapter.load_config(args.config))
        if args.command == "adapter":
            data = sys.stdin.buffer.read(scenarioadapter.MAX_REQUEST + 1)
            require(len(data) <= scenarioadapter.MAX_REQUEST, "Scenario request exceeds adapter input bound")
            result = adapter.request(loads_strict(data, "Scenario request", scenarioadapter.MAX_REQUEST))
        elif args.command == "submit":
            result = {"schema": "af.perf-scenario-service/1", "status": adapter.submit_pending()}
        else:
            adapter.verify_pending()
            result = {"schema": "af.perf-scenario-service/1", "status": "verification pass finished"}
    data = document_bytes(result)
    require(len(data) <= 65536, "Scenario adapter response exceeds output bound")
    sys.stdout.buffer.write(data)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ContractError, OSError, ValueError) as error:
        sys.stderr.write(str(error)[:4096] + "\n")
        sys.exit(2)
