#!/usr/bin/env python3
"""Offline, advisory checks of an AF explain export and an operator checklist.

This is not a scheduler, receipt verifier, plan approval, or acceptance decision.
It never invokes AF, Git, a provider, or a command from an input document.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import time


def require(condition, message):
    if not condition:
        raise ValueError(message)


def integer(value, label):
    require(type(value) is int and value >= 0, label + " must be a nonnegative integer")
    return value


def digest(value):
    require(isinstance(value, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", value),
            "expected a sha256 artifact identity")
    return value


def nonempty_name(value):
    require(isinstance(value, str) and value.strip(), "expected a nonempty name")
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key: " + key)
        result[key] = value
    return result


def read(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(16 * 1024 * 1024 + 1)
    require(len(raw) <= 16 * 1024 * 1024, "input exceeds 16 MiB")
    value = json.loads(raw, object_pairs_hook=unique_object)
    require(isinstance(value, dict), "input must be a JSON object")
    return value, "sha256:" + hashlib.sha256(raw).hexdigest()


def operator(node):
    op = node["operator"]
    return op.get("operator", {}) if op["kind"] == "primitive" else {}


def report(inspection, checklist, now, review_plan=None):
    require(inspection["schema"] == "af/task-inspection@11", "expected af task explain --json (@11)")
    require(checklist["schema"] == "af-preflight-checklist/1", "unsupported checklist schema")
    require(set(checklist) <= {"schema", "plan_id", "original_source_snapshot_id",
                             "retained_candidate_snapshot_id", "criteria"},
            "unknown checklist field")
    plan, graph = inspection["plan"], inspection["graph"]
    nonempty_name(inspection["task_id"])
    require(graph["schema"] == "af.compiled-task/1", "unsupported compiled graph")
    require(plan["task_revision_id"] == inspection["revision_id"], "plan/revision mismatch")
    digest(inspection["plan_id"])
    digest(checklist["plan_id"])
    nodes, order = graph["nodes"], graph["order"]
    require(len(order) == len(set(order)) and set(order) == set(nodes), "invalid graph order")
    positions = {node: index for index, node in enumerate(order)}
    for name, node in nodes.items():
        addresses = list(node["inputs"].values()) + [c["source"] for c in node["conditions"]]
        for address in addresses:
            require(address["node"] in nodes, "unknown graph input producer")
            require(address["port"] in nodes[address["node"]]["contract"]["outputs"],
                    "unknown graph output port")
            require(positions[address["node"]] < positions[name], "graph is not in dependency order")

    issues = []

    def issue(code, detail, status="conflict"):
        issues.append({"status": status, "code": code, "detail": detail})

    if checklist["plan_id"] != inspection["plan_id"]:
        issue("stale_checklist", "Checklist is bound to a different plan.")
    source = plan["inputs"].get("source", {})
    expected = digest(checklist["original_source_snapshot_id"])
    actual = source.get("snapshot_id")
    if source.get("artifact_type") != "af/SourceTree@1" or not actual:
        issue("source_unavailable", "Plan does not expose a source Snapshot.", "unknown")
    elif actual != expected:
        issue("source_changed", "Captured source differs from the declared original base.")
    candidate = checklist.get("retained_candidate_snapshot_id")
    if candidate is not None:
        digest(candidate)
        if candidate == actual:
            issue("candidate_as_source", "Retained candidate was selected as the source; inspect review base.")
        issue("replay_unverified", "Export cannot prove retained candidate replay into the next implementation.",
              "unknown")
    diff_status = "not_verified"
    if review_plan is not None:
        require(review_plan["schema"] == "af/review-plan@1", "expected af review plan --json (@1)")
        resolved, subject = review_plan["resolved"], review_plan["subject"]
        if not candidate:
            issue("candidate_unbound", "Diff export needs an explicit retained candidate Snapshot in the checklist.", "unknown")
        elif (resolved["base_snapshot_id"] != expected
              or resolved["candidate_snapshot_id"] != candidate):
            issue("diff_identity_mismatch", "Diff export does not name the expected original base and retained candidate.")
        else:
            require(subject["kind"] == "diff" and type(subject["empty"]) is bool
                    and isinstance(subject["changed_paths"], list), "invalid diff summary")
            if subject["empty"] or not subject["changed_paths"]:
                issue("empty_intended_diff", "Matched diff export has no changed paths.")
                diff_status = "empty"
            else:
                diff_status = "nonempty_export"
    elif candidate:
        issue("diff_unavailable", "Supply the matched af review plan JSON to inspect the retained diff.", "unknown")

    limits = plan["limits"]
    cap = integer(limits["tokens"], "token cap")
    charged = inspection["chargeable_tokens"]
    require(isinstance(charged, str) and re.fullmatch(r"0|[1-9][0-9]*", charged),
            "chargeable_tokens must be an exact decimal string")
    charged = int(charged)
    attempts = integer(inspection["attempts"], "begun attempts")
    max_attempts = integer(limits["max_attempts"], "attempt cap")
    reserve = {k: integer(limits["verification"][k], "verification " + k)
               for k in ("tokens", "attempts", "wall_ms")}
    deadline = integer(limits["deadline_unix_ms"], "deadline")
    rows = []
    for name, allowance in sorted(graph["allowances"].items()):
        require(name in nodes, "allowance references unknown node")
        allowance = {k: integer(allowance[k], k) for k in
                     ("tokens_per_attempt", "max_attempts", "wall_ms_per_attempt", "verification_attempts")}
        node = nodes[name]
        op = operator(node)
        role = graph["slots"].get(op.get("slot"), {}).get("role")
        category = ("provider_admission" if node["operator"]["kind"] == "provider_admission"
                    else role or op.get("op") or node["operator"]["kind"])
        rows.append({"node": name, "category": category, "operation": op.get("op"), **allowance})
    children = graph.get("owned_children", {})
    scopes = graph.get("token_scopes", {})
    if children or graph.get("review_integration") or graph.get("experimental_slots"):
        issue("dynamic_budget_unknown", "Dynamic child/Integration/experiment bounds are reported, "
              "but remaining admission is not reconstructed.", "unknown")
    if charged > cap or attempts > max_attempts:
        issue("budget_exceeded", "Recorded charge or begun Attempts exceed the captured cap.")
    if deadline <= now:
        issue("deadline_expired", "Captured absolute deadline has expired.")
    pristine = (attempts == 0 and charged == 0 and inspection["phase"]["kind"] in ("submitted", "ready")
                and not inspection["execution_records"])
    if pristine:
        if reserve["tokens"] > cap or reserve["attempts"] > max_attempts:
            issue("reserve_exceeds_cap", "Configured verification reserve exceeds total allowance.")
        for row in rows:
            if row["operation"] != "worker" or row["verification_attempts"]:
                continue
            if (row["tokens_per_attempt"] + reserve["tokens"] > cap
                    or 1 + reserve["attempts"] > max_attempts):
                issue("reservation_plus_reserve", row["node"] +
                      " cannot fit alongside the full configured verification reserve.")
    else:
        issue("remaining_admission_unknown", "Charge headroom excludes live reservations; current required "
              "verification and retry eligibility remain AF's decision.", "unknown")

    criteria = checklist["criteria"]
    require(isinstance(criteria, list), "criteria must be a list")
    if not criteria:
        issue("empty_checklist", "No requirements have an explicit evidence mapping.", "unknown")
    seen, coverage, evidence = set(), set(), []
    for criterion in criteria:
        require(set(criterion) <= {"id", "requirement", "obligation", "producer", "consumer", "check_name"},
                "unknown criterion field")
        cid = criterion["id"]
        require(isinstance(cid, str) and cid.strip() and cid not in seen, "duplicate or empty criterion id")
        seen.add(cid)
        require(isinstance(criterion["requirement"], str) and criterion["requirement"].strip(),
                "criterion needs requirement text")
        obligation = criterion["obligation"]
        require(isinstance(obligation, str) and obligation.strip(), "criterion needs an obligation name")
        if "check_name" in criterion:
            require(isinstance(criterion["check_name"], str) and criterion["check_name"].strip(),
                    "criterion needs a check name")
        producer, consumer = criterion["producer"], criterion["consumer"]
        for address in (producer, consumer):
            require(set(address) == {"node", "port"}, "evidence address needs exactly node and port")
            require(all(isinstance(address[k], str) and address[k].strip() for k in ("node", "port")),
                    "evidence node and port must be nonempty strings")
        start = len(issues)
        if obligation not in plan["acceptance"]:
            issue("unknown_obligation", cid + ": obligation is not in captured acceptance.")
        else:
            coverage.add(obligation)
        if producer["node"] not in nodes or consumer["node"] not in nodes:
            issue("missing_evidence_node", cid + ": producer or consumer is absent from the graph.")
        else:
            p, c = nodes[producer["node"]], nodes[consumer["node"]]
            if producer["port"] not in p["contract"]["outputs"]:
                issue("missing_evidence_port", cid + ": producer output does not exist.")
            if c["inputs"].get(consumer["port"]) != producer:
                issue("evidence_not_consumed", cid + ": consumer input is not bound to that producer output.")
            if positions[producer["node"]] >= positions[consumer["node"]]:
                issue("evidence_after_consumer", cid + ": evidence is not produced before its consumer.")
            if operator(c).get("op") not in ("verify", "fix_verify"):
                issue("consumer_not_verifier", cid + ": consumer is not a verification Worker.")
            target = graph["coverage"].get(obligation, {}).get("node")
            # Follow only data paths that carry this verifier toward coverage. Optional
            # or conditional paths establish a connection, not guaranteed evidence use.
            feeds = {consumer["node"]}
            for current in order:
                if any(a["node"] in feeds for a in nodes[current]["inputs"].values()):
                    feeds.add(current)
            pending, ancestors = [target] if target in feeds else [], set()
            optional_path, conditional_path = False, False
            while pending:
                current = pending.pop()
                if current in ancestors:
                    continue
                ancestors.add(current)
                current_node = nodes[current]
                conditional_path |= bool(current_node["conditions"])
                if current == consumer["node"]:
                    continue
                for port, address in current_node["inputs"].items():
                    if address["node"] in feeds:
                        optional_path |= current_node["contract"]["inputs"][port]["optional"]
                        pending.append(address["node"])
            if consumer["node"] not in ancestors:
                issue("verifier_not_in_acceptance", cid + ": verifier does not feed this obligation's coverage.")
            if optional_path:
                issue("coverage_edge_optional", cid + ": coverage uses an optional input; sufficiency needs review.", "unknown")
            if conditional_path:
                issue("conditional_coverage", cid + ": verifier/coverage path is conditional; reachability needs review.", "unknown")
            if operator(p).get("op") == "check" and p["inputs"].get("source") != c["inputs"].get("source"):
                issue("evidence_source_mismatch", cid + ": source addresses differ; current-Snapshot affinity needs review.", "unknown")
            if "check_name" in criterion:
                if criterion["check_name"] not in operator(p).get("checks", []):
                    issue("missing_named_check", cid + ": named check is not declared by the producer.")
            elif operator(p).get("op") == "check":
                issue("unspecified_check", cid + ": name the specific check expected to produce this proof.", "unknown")
            if p["conditions"]:
                issue("conditional_evidence", cid + ": producer is conditional; reachability needs review.", "unknown")
        evidence.append({**criterion, "structural_status": "connected" if len(issues) == start else "needs_review"})
    for obligation in sorted(set(plan["acceptance"]) - coverage):
        issue("unmapped_obligation", obligation + ": no checklist criterion names this acceptance obligation.", "unknown")

    return {
        "schema": "af-preflight-report/1", "advisory_only": True,
        "status": "needs_review" if issues else "no_structural_conflicts",
        "task_id": inspection["task_id"], "plan_id": inspection["plan_id"],
        "revision_id": inspection["revision_id"], "observed_at_unix_ms": now,
        "lineage": {"expected_original_source_snapshot_id": expected, "source_snapshot_id": actual,
                    "retained_candidate_snapshot_id": candidate, "intended_diff": diff_status,
                    "candidate_replay": "not_verified"},
        "budget": {"token_cap": str(cap), "charged_tokens": str(charged),
                   "headroom_before_live_reservations": str(cap - charged),
                   "attempt_cap": max_attempts, "begun_attempts": attempts,
                   "configured_verification_reserve": reserve,
                   "deadline_unix_ms": deadline, "remaining_wall_ms": max(0, deadline - now),
                   "declared_allowances": rows, "owned_child_templates": children, "token_scopes": scopes,
                   "review_integration": graph.get("review_integration"),
                   "experimental_slots": graph.get("experimental_slots", {})},
        "acceptance": plan["acceptance"], "criteria": evidence, "issues": issues,
        "limitations": ["Exports are not authenticated against the Store and may be stale.",
                        "Configured reserve is not a recomputation of still-required verification.",
                        "Per-node caps are not predictions or additive remaining spend; branches and scopes overlap.",
                        "Checklist completeness and semantic evidence sufficiency require review.",
                        "This report grants no approval, dispatch, acceptance, or delivery authority."],
    }


def render(value):
    budget = value["budget"]
    lines = ["ADVISORY PREFLIGHT — " + value["status"],
             "Task: " + value["task_id"], "Plan: " + value["plan_id"],
             "Source: " + str(value["lineage"]["source_snapshot_id"]),
             "Expected original source: " + value["lineage"]["expected_original_source_snapshot_id"],
             "Retained candidate: " + str(value["lineage"]["retained_candidate_snapshot_id"]),
             "Intended diff: " + value["lineage"]["intended_diff"] + "; candidate replay: not_verified",
             "Tokens: " + budget["charged_tokens"] + " charged / " + budget["token_cap"] + " cap; " +
             budget["headroom_before_live_reservations"] + " headroom BEFORE live reservations",
             "Attempts: {} begun / {} cap; deadline: {} Unix ms; {} ms remaining".format(
                 budget["begun_attempts"], budget["attempt_cap"], budget["deadline_unix_ms"], budget["remaining_wall_ms"]),
             "Configured verification reserve: " + json.dumps(budget["configured_verification_reserve"]),
             "Declared per-node allowances (not remaining spend):"]
    for row in budget["declared_allowances"]:
        lines.append("  {node}: {category}; {tokens_per_attempt} tokens/Attempt, "
                     "{max_attempts} Attempts, {wall_ms_per_attempt} ms/Attempt".format(**row))
    for field in ("owned_child_templates", "token_scopes", "review_integration", "experimental_slots"):
        if budget[field]:
            lines.append(field + " (declared, not additive spend): " + json.dumps(budget[field], sort_keys=True))
    lines.append("Evidence mappings:")
    for row in value["criteria"]:
        lines.append("  " + row["id"] + ": " + row["requirement"] + " [" + row["structural_status"] + "]")
        lines.append("    {node}.{port}".format(**row["producer"]) + " -> " +
                     "{node}.{port}".format(**row["consumer"]) + " => " + row["obligation"])
    lines.extend("  " + row["status"].upper() + " " + row["code"] + ": " + row["detail"]
                 for row in value["issues"])
    lines.extend("NOTE: " + note for note in value["limitations"])
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inspection", required=True, help="af task explain TASK_ID --json export")
    parser.add_argument("--checklist", required=True, help="operator-authored evidence mapping")
    parser.add_argument("--review-plan", help="optional af review plan --json export for the retained diff")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--now-unix-ms", type=int, help="explicit observation clock for reproducible reports")
    args = parser.parse_args()
    try:
        inspection, ihash = read(args.inspection)
        checklist, chash = read(args.checklist)
        now = integer(args.now_unix_ms if args.now_unix_ms is not None else time.time_ns() // 1000000, "clock")
        review_plan, rhash = read(args.review_plan) if args.review_plan else (None, None)
        value = report(inspection, checklist, now, review_plan)
        value["input_file_hashes"] = {"inspection": ihash, "checklist": chash}
        if rhash:
            value["input_file_hashes"]["review_plan"] = rhash
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError) as error:
        value = {"schema": "af-preflight-report/1", "advisory_only": True,
                 "status": "invalid_input", "error": str(error)}
        print(json.dumps(value) if args.json else "Invalid preflight input: " + str(error))
        return 1
    print(json.dumps(value, indent=2) if args.json else render(value))
    return 2 if value["issues"] else 0


if __name__ == "__main__":
    sys.exit(main())
