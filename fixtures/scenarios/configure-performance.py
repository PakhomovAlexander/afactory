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
    parser.add_argument("--publication", help="optional reviewed af/performance-publication-activation@1; requires installed workspace scope")
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
            if pin["schema"] != "af/scenario-template-pin@2":
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
    policy = dict(schema="af/scenario-policy@5", adapters={"lab":dict(executable=launcher["executable"],
        digest=launcher["digest"], operations=["experiment_submit_v1", "observe"], destinations=["native:claw"],
        max_wall_ms=30000, max_output_bytes=65536)}, task_plans=[], task_templates=sorted(set(digests)),
        decision_keys={}, file_roots=[])
    policy["performance_publications"] = {}
    if args.publication:
        activation = read(args.publication)
        if set(activation) != {"schema", "adapter", "scope", "title"} or activation["schema"] != "af/performance-publication-activation@1":
            raise ValueError("expected exact performance publication activation")
        stages = {s["id"]: s for s in root["steps"]}
        if not {"candidate", "verification", "report", "comparison"} <= stages.keys():
            raise ValueError("no-worthwhile loop has no candidate publication authority")
        scope, adapter = activation["scope"], activation["adapter"]
        destination = "github:" + scope["repository"]
        if (scope["evidence_adapter"] != "lab" or scope["evidence_destination"] != "native:claw"
                or adapter["operations"] != ["publish_performance_pr_v1"] or adapter["destinations"] != [destination]
                or not pathlib.Path(adapter["executable"]).is_absolute()
                or re.fullmatch(r"sha256:[0-9a-f]{64}", adapter["digest"]) is None):
            raise ValueError("publication activation must pin only the exact workspace operation/destination")
        template = stages["candidate"]["operation"]["template"]
        if (scope["repo"], scope["task_state"], scope["source_commit"], scope["source_content_digest"]) != (
                template["repo"], template["state"], template["authority_commit"], template["source_content_digest"]):
            raise ValueError("publication source differs from native candidate template")
        grant = dict(adapter="performance-publication", operation="publish_performance_pr_v1", destination=destination)
        step_ref = lambda name: dict(source="step", step=name, port="result")
        project = lambda kind: dict(source="project", value=step_ref("candidate"), projection=dict(kind=kind, **({"port":"snapshot"} if kind == "task_output_snapshot" else {})))
        root["steps"].append(dict(id="publish-performance", operation=dict(kind="action", grant=grant,
            requires_decision=False, required_condition=None),
            inputs=dict(subject=project("task_result_id"), revision=project("task_output_snapshot"),
                body=dict(source="literal", value=dict(type="text", value=json.dumps(dict(schema="af/performance-pr-content@1", title=activation["title"]), sort_keys=True, separators=(",", ":")))),
                candidate=step_ref("candidate"), verification=step_ref("verification"), report=step_ref("report"), measurement=step_ref("comparison")),
            outputs=dict(result="action"), when=None, max_attempts=1, timeout_ms=30000, on_timeout="fail"))
        root["authority"]["grants"].append(grant)
        root["outputs"]["publication"] = "action"
        root["result"]["publication"] = step_ref("publish-performance")
        policy["adapters"]["performance-publication"] = adapter
        policy["performance_publications"]["performance-publication"] = scope
    ant = dict(schema="af/ant@7", id="performance-native", scenario=copy.deepcopy(bundle), inputs={},
        schedule=dict(kind="daily", timezone="UTC", hour=3, minute=0, start_unix_ms=1793664000000),
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
