#!/usr/bin/env python3
"""Check (default) or rewrite `schemas/performance-*-v<N>.json` from the laboratory's contracts.

The JSON Schemas are generated from the same definitions the readers validate with, so a
difference means a contract changed without its schema, or the reverse.
"""
import argparse
import json
import os
import sys

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from perflab import contracts  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(HERE))
SCHEMAS = os.path.join(ROOT, "schemas")


def rendered():
    return dict((name, (json.dumps(doc, indent=2, ensure_ascii=False) + "\n").encode("utf-8"))
                for name, doc in contracts.schema_documents().items())


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--write", action="store_true", help="rewrite the schema files")
    args = parser.parse_args(argv)
    stale = []
    for name, data in sorted(rendered().items()):
        path = os.path.join(SCHEMAS, name)
        current = None
        if os.path.exists(path):
            with open(path, "rb") as stream:
                current = stream.read()
        if current != data:
            stale.append(name)
            if args.write:
                with open(path, "wb") as stream:
                    stream.write(data)
    if stale and not args.write:
        sys.stderr.write("stale performance schemas: %s\nrun python3 scripts/performance/"
                         "schemas.py --write\n" % ", ".join(stale))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
