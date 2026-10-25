#!/usr/bin/env python3
"""Finish the stable Performance check using only the trusted control's code and policy."""
import argparse
import json
import os
import sys
import uuid

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from perflab import status  # noqa: E402
from perflab.values import ContractError  # noqa: E402


def command(level, title, message):
    """Escape data before emitting a GitHub Actions annotation command."""
    def escape(value):
        return value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    return "::%s title=%s::%s" % (level, escape(title).replace(",", "%2C").replace(":", "%3A"),
                                  escape(message))


def outputs(path, values):
    with open(path, "a", encoding="utf-8") as stream:
        for key, value in values.items():
            delimiter = "af_perf_" + uuid.uuid4().hex
            while delimiter in value:
                delimiter = "af_perf_" + uuid.uuid4().hex
            stream.write("%s<<%s\n%s\n%s\n" % (key, delimiter, value, delimiter))


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
    label, message, annotations = status.presentation(
        args.schedule, args.reports, os.environ["GITHUB_REPOSITORY"],
        int(os.environ["GITHUB_RUN_ID"]), int(os.environ["GITHUB_RUN_ATTEMPT"]),
        os.environ["GITHUB_SHA"], qualified, details, mode,
        server_url=os.environ.get("GITHUB_SERVER_URL", "https://github.com"))
    # Summary data is never printed as raw workflow commands: a case name or refusal detail
    # may contain a newline followed by an Actions command.
    print("Performance report: " + label)
    for level, annotation in annotations:
        print(command(level, "Performance report", annotation))
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as stream:
            stream.write(message)
    if os.environ.get("GITHUB_OUTPUT"):
        outputs(os.environ["GITHUB_OUTPUT"], {"report_label": label, "report_summary": message})
    return 0 if mode == "advisory" or qualified else 1


if __name__ == "__main__":
    sys.exit(main())
