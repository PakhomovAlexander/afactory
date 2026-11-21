#!/usr/bin/env python3
"""Probe the capabilities exposed by this native AF test adapter."""
import json
import sys


def main():
    if sys.argv[1:] != ["capabilities"]:
        raise SystemExit("this native adapter only supports the capability probe")
    request = json.load(sys.stdin)
    print(json.dumps({
        "engine": {"name": "af-native", "build": "unprobed"},
        "capabilities": ["same_host_writer_replacement", "separate_dependency_vm"],
        "reason": "The native harness runs Task subprocesses against a local Store. This adapter "
                  "cannot place Attempts on remote Workers or independently deliver a stale completion.",
        "requested_profile": request["input"]["profile"],
    }))


if __name__ == "__main__":
    main()
