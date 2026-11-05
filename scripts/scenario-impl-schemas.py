#!/usr/bin/env python3
"""Render/check implementation adapter schema parity; no machine installation or publication."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys
sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("impl_schema_install", ROOT / "scripts/install-scenario-impl-adapter.py")
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--write", action="store_true")
args = parser.parse_args()
for name, schema in installer.module().impl_schema_documents().items():
    path = ROOT / "schemas" / name
    rendered = json.dumps(schema, indent=2) + "\n"
    if args.write:
        path.write_text(rendered)
    elif path.read_text() != rendered:
        raise SystemExit("schema parity differs: " + name)
