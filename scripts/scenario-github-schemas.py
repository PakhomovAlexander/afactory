#!/usr/bin/python3 -IS
"""Render/check the installed GitHub adapter's versioned structural schemas."""
import importlib.util
import json
from pathlib import Path
import sys
sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("github_schema", ROOT / "scripts/scenario-github-adapter.py")
ADAPTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ADAPTER)
for name, value in ADAPTER.schema_documents().items():
    path = ROOT / "schemas" / name
    rendered = json.dumps(value, indent=2, sort_keys=True) + "\n"
    if sys.argv[1:] == ["--write"]:
        path.write_text(rendered)
    elif sys.argv[1:] or path.read_text() != rendered:
        raise SystemExit("GitHub adapter schema parity drift: " + name)
