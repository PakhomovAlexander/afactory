#!/usr/bin/env python3
from pathlib import Path

text = Path("README.md").read_text(encoding="utf-8")
assert text.startswith("# Development-flow smoke workspace\n")
assert "smoke" in text
for number, line in enumerate(text.splitlines(), 1):
    assert line == line.rstrip(), f"trailing whitespace on README.md:{number}"
print("documentation smoke check passed")
