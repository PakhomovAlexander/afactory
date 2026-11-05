#!/usr/bin/env python3
"""Author reusable impl packages and a real native Scenario bundle. Never run models or publish."""
import argparse
import copy
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent.parent
TEAM = {"implementer": ("gpt-6.1-sol", "medium"), "correctness": ("gpt-6.1-sol", "high"),
        "bugs": ("gpt-6.1-terra", "high"), "evaluator": ("gpt-6-astra", "medium")}


def read(path):
    raw = Path(path).read_bytes()
    if len(raw) > 1048576:
        raise ValueError("authoring input exceeds 1 MiB")
    return json.loads(raw)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()



def literal(text):
    return {"source": "literal", "value": {"type": "text", "value": text}}


def step_ref(step):
    return {"source": "step", "step": step, "port": "result"}


def project(step, kind, **kwargs):
    return {"source": "project", "value": step_ref(step), "projection": {"kind": kind, **kwargs}}


def step(name, operation, inputs, port, timeout=604800000):
    return dict(id=name, operation=operation, inputs=inputs, outputs={"result": port}, when=None,
                max_attempts=1, timeout_ms=timeout, on_timeout="fail")


def bundle(pin, destination, title, body, owner="owner", adapter="impl", repairs=2):
    if type(repairs) is not int or not 0 <= repairs <= 2:
        raise ValueError("impl permits zero, one or two bounded implementation repairs")
    if pin["schema"] != "af/scenario-template-pin@1" or re.fullmatch(r"sha256:[0-9a-f]{64}", pin["template_id"]) is None:
        raise ValueError("expected exact native template-pin receipt")
    template = copy.deepcopy(pin["template"])
    doc = template["document"]
    if (doc["kind"] != "implement" or doc["pipeline"] != {"name": "impl/implementation", "fallback": "refuse"}
            or doc["verification"] != "evaluation" or doc["requirements"].get("scenario_mode") != "impl"):
        raise ValueError("impl requires its exact implementation pipeline and executable reviewer contract")
    if re.fullmatch(r"github:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", destination) is None:
        raise ValueError("exact GitHub machine destination required")
    grants = [dict(adapter=adapter, operation=op, destination=destination)
              for op in ("impl_prepare_v1", "impl_publish_v1", "impl_cleanup_v1", "observe")]
    by_op = {g["operation"]: g for g in grants}
    def action(op, decision=False, condition=None):
        return dict(kind="action", grant=by_op[op], requires_decision=decision, required_condition=condition)
    def wait(condition):
        return dict(kind="wait", grant=by_op["observe"], condition=condition)
    def projection(value, kind, **kwargs):
        return dict(source="project", value=value, projection=dict(kind=kind, **kwargs))
    def choose(condition, passed, failed):
        return dict(source="select", condition=condition, passed=passed, failed=failed)
    publication_body = literal(canonical(dict(schema="af/scenario-impl-publication@1", title=title, body=body)).decode())
    steps = []
    task_value = prepared_value = outcome_value = publication_value = None
    for iteration in range(repairs + 1):
        suffix = "" if iteration == 0 else "-repair-" + str(iteration)
        impl, prep, decision, pub, observed = [name + suffix for name in ("implement", "prepare", "publication-owner", "publish", "outcome")]
        condition = projection(outcome_value, "impl_repair_needed") if iteration else None
        bindings = {"source": {"port": "snapshot", "artifact_type": "af/SourceTree@1"}} if iteration else {}
        task_inputs = dict(source=task_value, scenario_evidence=outcome_value) if iteration else {}
        task = step(impl, dict(kind="fresh_task", template=template, bindings=bindings), task_inputs, "task", doc["limits"]["wall_ms"])
        prepare_inputs = dict(subject=project(impl, "task_result_id"), revision=project(impl, "task_output_snapshot", port="snapshot"),
                              body=publication_body, candidate=step_ref(impl))
        if iteration:
            prepare_inputs.update(previous=prepared_value, repair=outcome_value)
        prepare = step(prep, action("impl_prepare_v1"), prepare_inputs, "action")
        obj = dict(subject=project(prep, "impl_subject"), revision=project(prep, "impl_revision"))
        owner_step = step(decision, dict(kind="decision", owner=owner), obj, "decision")
        publish = step(pub, action("impl_publish_v1", True), dict(**obj, body=project(prep, "impl_body"), prepared=step_ref(prep), decision=step_ref(decision)), "action")
        outcome = step(observed, wait("implementation_outcome"), obj, "observation")
        for item in (task, prepare, owner_step, publish, outcome):
            item["when"] = condition
            steps.append(item)
        task_value = choose(condition, step_ref(impl), task_value) if iteration else step_ref(impl)
        prepared_value = choose(condition, step_ref(prep), prepared_value) if iteration else step_ref(prep)
        publication_value = choose(condition, step_ref(pub), publication_value) if iteration else step_ref(pub)
        outcome_value = choose(condition, step_ref(observed), outcome_value) if iteration else step_ref(observed)
    obj = dict(subject=projection(prepared_value, "impl_subject"), revision=projection(prepared_value, "impl_revision"))
    steps += [step("ready", wait("implementation_ready"), obj, "observation"),
              step("merged", wait("merged"), obj, "observation"),
              step("cleanup", action("impl_cleanup_v1", condition="merged"), dict(**obj, body=projection(prepared_value, "impl_body"),
                  prepared=prepared_value, evidence=step_ref("merged")), "action")]
    root = dict(schema="af/scenario@4", inputs={}, outputs={"implementation": "task", "publication": "action", "merged": "observation", "cleanup": "action"},
        steps=steps, result={"implementation": task_value, "publication": publication_value, "merged": step_ref("merged"), "cleanup": step_ref("cleanup")},
        completion=[project("merged", "observation_satisfied")],
        authority=dict(task_plans=[], task_templates=[pin["template_id"]], grants=grants, decision_owners=[owner]),
        limits=dict(max_tokens=template["max_tokens"] * (repairs + 1), max_wall_ms=doc["limits"]["wall_ms"] * (repairs + 1) + 604800000, max_dispatches=256), admission=None)
    return dict(schema="af/scenario-bundle@4", root="impl", definitions={"impl": root})


def packages(output, af, checks):
    """Generate independent packages, reusing native reviewed Pipeline/Worker interfaces."""
    output = Path(output)
    output.mkdir(mode=0o700)
    entries = {}
    instructions = {
        "implementer": """# Implement one bounded change

Implement the Task requirements in the captured source. This is a standalone CLI/project;
its name does not authorize adding an af subcommand. Use the repository guidance and tests.
The required local Gate and two required reviewers check the sealed candidate, and the
independent correctness reviewer builds, privately installs and executes it. Address every
acceptance item. Report all failed or unavailable checks exactly. Never publish or use
credentials. Keep the declared Attempt and Task budgets; unfinished work is not complete.
""",
        "correctness": """# Executable correctness verifier

Independently verify the complete candidate against the requirements and base. You have the
native execute-checks capability in a private disposable sandbox. The candidate is already
applied; do not edit its source. Build the standalone CLI with the pinned toolchain, install
it under a private sandbox directory (for Rust, cargo install --path . --root PATH), and
execute meaningful focused tests and installed-binary acceptance cases. Keep every build and
install artifact inside your sandbox. Inspect and exercise error paths and lifecycle behavior.

Return both declared ports: the unchanged flat Reviewer Result in result, and
ImplementationTestEvidence@1 in testing. The testing artifact names the exact candidate Snapshot,
records build/install/test commands, exit statuses and bounded public execution evidence. Its
status is passed only when every command ran successfully and all three purposes are covered.
Use failed or unavailable otherwise; never invent a successful command receipt.

A prose review, reading the implementer's report, or observing the Gate is insufficient.
Run actual commands yourself. State their exact commands, exit status and the behavior each
proves in evidence. If execution is unavailable, fails, or cannot establish an acceptance
item, file a blocking Finding naming the missing proof. Never claim a pass for an unrun
command. Never print credentials or publish. Your source must seal unchanged; generated
private build/install files are disposable, never a candidate or a Task result.
""",
        "bugs": """# Required bugs reviewer

Independently inspect the complete sealed candidate and requirements, including recovery,
security, edge cases and regressions. Trace concrete reachable defects and return the native
Reviewer Result contract. Inspect actual code and tests; do not accept the implementer's
summary as evidence. Missing acceptance work blocks acceptance. Never publish or edit source.
""",
        "evaluator": """# Independent acceptance evaluator

Judge the entire requirements contract on the exact candidate, local Gate receipts and the
public testing artifact from the required correctness reviewer. The correctness reviewer must have independently built, privately
installed and executed meaningful tests of the candidate. Missing executable evidence or
unresolved requirements means acceptance is not satisfied. Task acceptance permits preparation;
it does not establish publication, PR checks, human merge or Scenario completion.
"""}
    for role, (model, effort) in TEAM.items():
        src = ROOT / ".af/task-packages/kernel" / role
        dest = output / role
        shutil.copytree(src, dest)
        # Captured source is read-only under the real Gate; only the new package copies
        # receive write permission. Never chmod the shipped package or a shared inode.
        for path in [dest, *dest.rglob("*")]:
            path.chmod(path.stat().st_mode | (0o700 if path.is_dir() else 0o200))
        manifest = dest / "worker.toml"
        text = manifest.read_text()
        text = re.sub(r'^name = .*$', 'name = "impl/' + role + '"', text, count=1, flags=re.M)
        text = re.sub(r'^version = .*$', 'version = "1.0.0"', text, count=1, flags=re.M)
        text = re.sub(r'^provider_kind = .*$', 'provider_kind = "codex"', text, flags=re.M)
        text = re.sub(r'^model = .*$', 'model = "' + model + '"', text, flags=re.M)
        text = re.sub(r'^effort = .*$', 'effort = "' + effort + '"', text, flags=re.M)
        if role == "correctness":
            text = text.replace('effects = ["read-source"]', 'effects = ["read-source", "execute-checks"]')
            text = text.replace('[signature.retains]\n', '[signature.retains]\ntesting = ["source", "subject", "checks"]\n')
            text += '\n[signature.contract.outputs.testing]\nartifact_type = "af/ImplementationTestEvidence@1"\ncardinality = "one"\noptional = false\ncovers = []\n[signature.contract.outputs.testing.affinity]\nkind = "same_as"\ninput = "source"\n'
            shutil.copyfile(ROOT / 'schemas/implementation-test-evidence-v1.json', dest / 'outputs/testing.schema.json')
        if role == "evaluator":
            text = text.replace('af/EvaluationInput@2', 'af/ImplementationEvaluationInput@1')
            text = text.replace('result = ["checks", "requirements", "source"]', 'result = ["checks", "requirements", "source", "testing"]')
            text += '\n[signature.contract.inputs.testing]\nartifact_type = "af/ImplementationTestEvidence@1"\ncardinality = "one"\noptional = false\ncovers = []\n[signature.contract.inputs.testing.affinity]\nkind = "same_as"\ninput = "source"\n'
            shutil.copyfile(ROOT / 'schemas/implementation-evaluation-input-v1.json', dest / 'input.schema.json')
        manifest.write_text(text)
        (dest / "instructions.md").write_text(instructions[role])
        entries["impl/" + role] = dest
    for name, original in (("implementation", "implementation-reviewed"), ("review", "review-code")):
        dest = output / name
        dest.mkdir()
        text = (ROOT / ".af/task-packages/kernel" / original / "pipeline.toml").read_text()
        text = text.replace("kernel/implementation-reviewed", "impl/implementation").replace("kernel/review-code", "impl/review")
        for role in TEAM:
            text = text.replace("kernel/" + role, "impl/" + role)
        text = re.sub(r'^version = .*$', 'version = "1.0.0"', text, count=1, flags=re.M)
        text = text.replace('checks = ["kernel", "markdownlint"]', "checks = " + json.dumps(checks))
        if name == "review":
            # The evaluator shares the Gate's conditional scope with correctness. Its
            # required testing input therefore cannot be absent on a selected invocation.
            text = re.sub(r'^max_attempts = 8$', 'max_attempts = 10', text, count=1, flags=re.M)
            text = re.sub(r'(\[contract.inputs.requirements\][^[]*?)optional = true', r'\1optional = false', text, count=1)
            text += """
[contract.outputs.testing]
artifact_type = "af/ImplementationTestEvidence@1"
cardinality = "one"
optional = true
covers = []
[contract.outputs.testing.affinity]
kind = "same_as"
input = "source"
[outputs.testing]
kind = "node"
node = "correctness"
port = "testing"
[contract.outputs.evaluation]
artifact_type = "af/TaskEvaluation@1"
cardinality = "one"
optional = true
covers = []
[contract.outputs.evaluation.affinity]
kind = "same_as"
input = "source"
[outputs.evaluation]
kind = "node"
node = "evaluate_goal"
port = "result"
[slots.evaluator]
worker = "impl/evaluator"
role = "evaluate"
input_type = "af/ImplementationEvaluationInput@1"
output_type = "af/TaskEvaluation@1"
min_attempts = 1
max_attempts = 2
allow_local_replacement = true
independent_from = []
[[nodes]]
id = "evaluate_goal"
[nodes.operator]
op = "verify"
slot = "evaluator"
[nodes.inputs.checks]
kind = "node"
node = "checks"
port = "result"
[nodes.inputs.requirements]
kind = "input"
port = "requirements"
[nodes.inputs.source]
kind = "input"
port = "source"
[nodes.inputs.testing]
kind = "node"
node = "correctness"
port = "testing"
[nodes.when]
node = "checks"
outcome = "passed"
"""
        else:
            text = text.replace('af/EvaluationInput@2', 'af/ImplementationEvaluationInput@1')
            text = text.replace('[nodes.operator.bindings]\n', '[nodes.operator.bindings]\nevaluator = "evaluator"\n')
            begin = text.index('[[nodes]]\nid = "evaluate_goal"')
            end = text.index('[[nodes]]\nid = "accept_goal"', begin)
            text = text[:begin] + text[end:]
            text = text.replace('node = "evaluate_goal"\nport = "result"', 'node = "review"\nport = "evaluation"')
            text += """
[contract.outputs.testing]
artifact_type = "af/ImplementationTestEvidence@1"
cardinality = "one"
optional = true
covers = []
[contract.outputs.testing.affinity]
kind = "derived_from"
input = "source"
[outputs.testing]
kind = "node"
node = "review"
port = "testing"
"""
        if "remote_checks" in text:
            raise ValueError("impl packages require local Gates only")
        (dest / "pipeline.toml").write_text(text)
        entries["impl/" + name] = dest
    lines = ['# Review this native catalog with the consuming repository local code policy.',
             'schema = "af.task-catalog/2"', 'code_policy = ".af/code-policy.toml"', 'no_match = "refuse"',
             '', '[review]', 'generation = 2', 'gate = "major"', 'clean_rounds = 1', 'max_rounds = 2',
             'allow_targeted_repairs = false', '', '[independence]', 'command_workers_by_package = true',
             'distinct_principals = false', 'distinct_providers = false', 'distinct_models = false']
    for name, dest in sorted(entries.items()):
        # The native hasher is the only package pin writer; no hand-typed digest or placeholder.
        process = subprocess.run([str(af), "catalog", "digest", str(dest)], check=True, capture_output=True, text=True,
                                 env=dict(os.environ, AF_SELF_OFFLINE="1"))
        digest = process.stdout.strip()
        if re.fullmatch(r"sha256:[0-9a-f]{64}", digest) is None:
            raise ValueError("native package digest receipt differs")
        lines += ["", '[packages.' + json.dumps(name) + ']', 'version = "1.0.0"', 'digest = ' + json.dumps(digest),
                  'path = ' + json.dumps(str(dest))]
    lines += ["", "[providers]"] + [json.dumps("impl/" + role) + ' = "codex-personal"' for role in TEAM]
    lines += ["", "# Both are required; no default-light pipeline is selected.", "[review.reviewers]", 'bugs = "required"', 'correctness = "required"']
    (output / "catalog.toml").write_text("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="command", required=True)
    pkg = subs.add_parser("packages", help="create new consuming-repository packages and exact native catalog pins")
    pkg.add_argument("--output", required=True)
    pkg.add_argument("--af", required=True, type=Path)
    pkg.add_argument("--check", action="append", required=True)
    build = subs.add_parser("bundle", help="create impl bundle from a real native template pin")
    build.add_argument("--pin", required=True)
    build.add_argument("--destination", required=True)
    build.add_argument("--title", required=True)
    build.add_argument("--body-file", required=True)
    build.add_argument("--owner", default="owner")
    build.add_argument("--repairs", type=int, choices=range(3), default=2)
    build.add_argument("--output", required=True)
    args = parser.parse_args()
    if args.command == "packages":
        packages(args.output, args.af.resolve(strict=True), args.check)
    else:
        value = bundle(read(args.pin), args.destination, args.title, Path(args.body_file).read_text(), args.owner, repairs=args.repairs)
        with Path(args.output).open("x") as output:
            json.dump(value, output, indent=2)
            output.write("\n")


if __name__ == "__main__":
    main()
