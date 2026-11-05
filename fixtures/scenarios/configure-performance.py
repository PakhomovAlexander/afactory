#!/usr/bin/env python3
"""Render inert performance definitions from native template/launcher receipts; never run them."""
import argparse
import copy
import json
import pathlib
import re


def read(path):
    with open(path, "rb") as source:
        data = source.read(1048577)
    if len(data) > 1048576:
        raise ValueError("authoring input exceeds 1 MiB")
    return json.loads(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", required=True)
    parser.add_argument("--pins", required=True, help="object mapping stage names to native template-pin envelopes")
    parser.add_argument("--launcher", required=True)
    parser.add_argument("--configuration", required=True)
    parser.add_argument("--producer", required=True)
    parser.add_argument("--evidence-index", required=True)
    parser.add_argument("--compatibility", required=True)
    parser.add_argument("--admission", required=True, help="exact roadmap/identity/revision object")
    parser.add_argument("--output", required=True, help="absent authoring directory")
    args = parser.parse_args()
    bundle = read(args.bundle)
    pins = read(args.pins)
    launcher = read(args.launcher)
    if launcher["schema"] != "af.perf-scenario-launcher/1":
        raise ValueError("expected native launcher receipt")
    root = bundle["definitions"][bundle["root"]]
    for digest in (args.configuration, args.producer, args.evidence_index, args.compatibility, launcher["digest"]):
        if re.fullmatch(r"sha256:[0-9a-f]{64}", digest) is None:
            raise ValueError("authoring requires exact native digest receipts")
    if not pathlib.Path(launcher["executable"]).is_absolute():
        raise ValueError("installed launcher path must be absolute")
    digests = []
    for step in root["steps"]:
        kind = step["operation"]["kind"]
        if kind == "fresh_task":
            pin = pins[step["id"]]
            if pin["schema"] != "af/scenario-template-pin@1":
                raise ValueError("expected native template-pin envelope")
            if pin["template"]["max_tokens"] > step["operation"]["template"]["max_tokens"]:
                raise ValueError("native package allowance exceeds the reviewed example reservation")
            expected = step["operation"]["template"]["document"]
            actual = pin["template"]["document"]
            if actual["kind"] != expected["kind"] or actual["requirements"]["scenario_mode"] != expected["requirements"]["scenario_mode"]:
                raise ValueError("native template changed the reviewed stage kind or evidence branch")
            step["operation"]["template"] = pin["template"]
            digests.append(pin["template_id"])
        if step["id"] == "history":
            step["inputs"]["subject"]["value"]["value"] = args.evidence_index
            step["inputs"]["revision"]["value"]["value"] = args.compatibility
        if step["id"] == "experiment":
            body = json.loads(step["inputs"]["body"]["value"]["value"])
            body.update(configuration_digest=args.configuration, producer_digest=args.producer)
            step["inputs"]["body"]["value"]["value"] = json.dumps(body, sort_keys=True, separators=(",", ":"))
    root["authority"]["task_templates"] = sorted(set(digests))
    root["admission"] = read(args.admission)
    if set(root["admission"]) != {"roadmap", "identity", "revision"}:
        raise ValueError("admission must be the exact native roadmap identity and revision")
    policy = dict(schema="af/scenario-policy@4", adapters={"lab":dict(executable=launcher["executable"],
        digest=launcher["digest"], operations=["experiment_submit_v1", "observe"], destinations=["native:claw"],
        max_wall_ms=30000, max_output_bytes=65536)}, task_plans=[], task_templates=sorted(set(digests)),
        decision_keys={}, file_roots=[])
    ant = dict(schema="af/ant@4", id="performance-native", scenario=copy.deepcopy(bundle), inputs={},
        schedule=dict(timezone="UTC", hour=3, minute=0, start_unix_ms=1793664000000),
        owner="performance-owner", notification="local:performance-owner",
        execution=dict(overlap="skip", missed="skip", lateness_ms=60000, per_run=root["limits"],
                       rolling_tokens=96000000, rolling_window_ms=604800000))
    output = pathlib.Path(args.output)
    output.mkdir(mode=0o700)
    for name, doc in (("bundle.json", bundle), ("policy.json", policy), ("ant.json", ant), ("inputs.json", {})):
        path = output / name
        with path.open("x") as target:
            json.dump(doc, target, indent=2)
            target.write("\n")
        path.chmod(0o600)


if __name__ == "__main__":
    main()
