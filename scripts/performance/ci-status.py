#!/usr/bin/env python3
"""Finish the stable Performance check using only the trusted control's code and policy."""
import argparse
import json
import os
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from perflab import status  # noqa: E402
from perflab.values import ContractError  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schedule", required=True)
    parser.add_argument("--reports", required=True)
    args = parser.parse_args()
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    # An unreadable policy is an error, never an advisory fallback.
    policy = status.enforcement(root)
    mode = status.effective_enforcement(policy, os.environ["GITHUB_EVENT_NAME"])
    try:
        qualified, details = status.evaluate(
            args.schedule, args.reports, json.loads(os.environ["PERF_JOB_RESULTS"]),
            os.environ["GITHUB_REPOSITORY"], int(os.environ["GITHUB_RUN_ID"]),
            int(os.environ["GITHUB_RUN_ATTEMPT"]), os.environ["GITHUB_SHA"])
    except (ContractError, OSError, ValueError, KeyError) as error:
        qualified, details = False, ["evidence unavailable or invalid: %s" % error]
    result = "qualified pass" if qualified else "not qualified"
    message = "### Performance check: %s (%s)\n\n%s\n" % (
        result, mode, "\n".join("- " + line.replace("\n", " ") for line in details)
        or "The full measured primary satisfies the required check.")
    print(message)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as stream:
            stream.write(message)
    return 0 if mode == "advisory" or qualified else 1


if __name__ == "__main__":
    sys.exit(main())
