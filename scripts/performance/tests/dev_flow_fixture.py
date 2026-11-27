"""Synthetic development-flow documents shared by oracle and native supervisor tests."""

import json
from pathlib import Path

from perflab import dev_flow


def document(root, treatment):
    evidence = Path(root) / treatment / "evidence"
    evidence.mkdir(parents=True)
    expected = dev_flow.COLD if treatment == "cold" else dev_flow.PRIMED
    rounds = []
    for index, (fresh, dirty) in enumerate(expected):
        units = []
        for crate in dev_flow.CRATES:
            if treatment == "primed" and index == 0:
                is_dirty = False
            elif index == 1:
                is_dirty = crate in ("smoke-logic", "smoke-app")
            else:
                is_dirty = True
            for target in range(2):
                units.append({"package": crate, "target": crate.replace("-", "_"),
                              "test_profile": bool(target), "fresh": not is_dirty})
        result = {
            "schema": "af/task-inspection@23",
            "state": {"phase": "finished", "outcome": dev_flow.OUTCOMES[index]},
            "result": {"domain_conclusion": dev_flow.OUTCOMES[index]},
            "review_rounds": [{"conclusion": dev_flow.REVIEW_CONCLUSIONS[index]}],
            "run_reports": [{"report": {"nodes": [
                {"node": "root.providers.admit0", "outcome": {"kind": "completed"}},
                {"node": "root.providers.admit1", "outcome": {"kind": "completed"}},
            ]}}],
        }
        if index:
            result["continues"] = {"task_id": dev_flow.TASKS[index - 1],
                                   "round": 1, "findings": 2}
        findings = None
        if index < 2:
            findings = {
                "schema": "af/task-findings@3",
                "round": {"conclusion": "changes_requested", "counts": {"major": 1}},
                "evaluation": {"outcome": "failed"},
            }
            (evidence / f"round-{index}-findings.json").write_text(json.dumps(findings))
        (evidence / f"round-{index}-run.json").write_text(json.dumps(result))
        (evidence / f"round-{index}-inspection.json").write_text(json.dumps(result))
        check_document = {
            "schema": "af/task-checks@4",
            "gate": {"status": "passed"},
            "receipt": {"checks": [
                {"name": "kernel", "status": "passed", "exit_code": 0},
                {"name": "markdownlint", "status": "passed", "exit_code": 0},
            ]},
        }
        (evidence / f"round-{index}-checks.json").write_text(json.dumps(check_document))
        artifacts = []
        for unit in units:
            artifacts.append({
                "reason": "compiler-artifact",
                "package_id": f"path+file:///fixture/{unit['package']}#0.1.0",
                "target": {"name": unit["target"]},
                "profile": {"test": unit["test_profile"]},
                "fresh": unit["fresh"],
            })
        (evidence / f"round-{index}-cargo.jsonl").write_text(
            "\n".join(json.dumps(artifact) for artifact in artifacts) + "\n")
        rounds.append({
            "round": index,
            "task_id": dev_flow.TASKS[index],
            "expected_exit": dev_flow.EXITS[index],
            "wall_ns": 1_000_000_000 + index,
            "result": result,
            "findings": findings,
            "cargo": {"fresh": fresh, "dirty": dirty, "units": units},
            "kernel": [{"elapsed_ms": 100 + index, "caches": []}],
            "check_document": check_document,
        })
    commands = []
    for index, label in enumerate(dev_flow.COMMAND_LABELS):
        commands.append({"index": index, "label": label, "output_overflow": False,
                         "exit_code": (dev_flow.EXITS[int(label[-1])] if label in
                                       ("run-smoke-0", "continue-smoke-1", "continue-smoke-2") else 0)})
        for stream in ("stdout", "stderr"):
            (evidence / f"command-{index:02d}-{label}.{stream}").write_text("")
    for name in ("final-semantic.stdout", "final-semantic.stderr"):
        (evidence / name).write_text("")
    source = {
        "crates/smoke-support/src/lib.rs": "pub fn base() -> i32 {\n    2\n}\n",
        "crates/smoke-logic/src/lib.rs": "pub fn calculate() -> i32 {\n    smoke_support::base() + 2\n}\n",
        "crates/smoke-app/src/lib.rs": "pub fn feature() -> i32 {\n    smoke_logic::calculate() + 1\n}\n",
    }
    invocations = []
    for model in ("gpt-6.2-sol", "gpt-6.1-terra"):
        invocations.extend({
            "role": "admission", "model": model,
            "binding": f"codex-main:{model}:high", "effort": "high",
        } for _ in range(3))
    for index in range(3):
        invocations.extend([
            {"role": "implementer"},
            {"role": "correctness", "finding_count": int(index < 2)},
            {"role": "evaluator", "outcome": "failed" if index < 2 else "passed"},
        ])
    return {
        "schema": "af.perf-dev-flow/2",
        "cache_treatment": treatment,
        "host": {"cpu_count": 4, "af_binary_sha256": "sha256:af",
                 "fixture_sha256": "sha256:fixture", "load_1m_start": 1.0,
                 "load_1m_end": 1.0},
        "metrics": {"accepted_wall_ns": 4_000_000_000,
                    "accepted_wall_ms": 4000,
                    "task_chain_wall_ns": 3_000_000_000,
                    "semantic_verification_ns": 1_000_000_000,
                    "final_tree_digest": "sha256:tree",
                    "final_semantics_oracle_sha256": "sha256:oracle",
                    "final_semantics_passed": True},
        "rounds": rounds,
        "final_source": source,
        "commands": commands,
        "mock": {
            "business_counts": dict(dev_flow.BUSINESS),
            "admission_count": 6,
            "invocations": invocations,
        },
        "evidence_root": str(evidence),
    }
