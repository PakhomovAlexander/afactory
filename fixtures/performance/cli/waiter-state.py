#!/usr/bin/env python3
"""Write one declared live waiter fixture with the measured side's public probe."""
import json
import subprocess
import sys


STATES = {
    "cli_state_10000": ("cli_wait_original_fixture", "cli_wait_original_fixture_v1", 10000),
    "cli_wait_small_100": ("cli_wait_fixture", "cli_wait_fixture_v1", 100),
    "cli_wait_small_10000": ("cli_wait_fixture", "cli_wait_fixture_v1", 10000),
    "cli_wait_large_10000": ("cli_wait_fixture", "cli_wait_fixture_v1", 10000),
    "cli_wait_small_history_100": ("cli_wait_fixture", "cli_wait_fixture_v1", 10000),
    "cli_wait_large_history_100": ("cli_wait_fixture", "cli_wait_fixture_v1", 10000),
}


def main(args):
    scratch, probe, state = args
    case, contract, events = STATES[state]
    request = {"schema": "af.perf-probe-request/2", "case_id": case,
               "instance_id": "%s__state_%s" % (case, state), "contract_id": contract,
               "parameters": {"state": state}, "repetitions": 1, "scratch": scratch,
               "seed": None}
    completed = subprocess.run([probe], input=json.dumps(request).encode(),
                               stdout=subprocess.PIPE, check=True)
    reply = json.loads(completed.stdout)
    if (reply.get("schema") != "af.perf-probe/1"
            or reply.get("case_id") != case or reply.get("contract_id") != contract
            or reply.get("instance_id") != request["instance_id"]
            or reply.get("correctness", {}).get("status") != "passed"
            or reply["correctness"].get("count") != events):
        raise ValueError("waiter fixture writer did not confirm its declared identity/count")
    sys.stdout.buffer.write(completed.stdout)


if __name__ == "__main__":
    main(sys.argv[1:])
