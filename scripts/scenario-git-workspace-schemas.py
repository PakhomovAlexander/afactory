#!/usr/bin/python3 -IS
"""Check/render the independent Git workspace contracts, without installing anything."""
import importlib.util
import json
from pathlib import Path
import sys
sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("workspace_installer", ROOT / "scripts/install-scenario-git-workspace.py")
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
for name, value in installer.module().workspace_schema_documents().items():
    path = ROOT / "schemas" / name
    rendered = json.dumps(value, indent=2, sort_keys=True) + "\n"
    if sys.argv[1:] == ["--write"]:
        path.write_text(rendered)
    elif sys.argv[1:] or path.read_text() != rendered:
        raise SystemExit("Workspace schema parity drift: " + name)
