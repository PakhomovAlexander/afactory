"""A rebuildable projection of explicit authoritative artifacts, never an archive in a prompt.

Native Store receipts authorize selected findings, History Captures and adoption. Finding
substance is bounded data, never instructions; raw logs and credentials are not retrieved.
"""
import os
import subprocess
import tempfile
from collections import Counter

from . import contracts, executor
from .scenariocontracts import INDEX_BYTES
from .values import canonical_digest, document_bytes, loads_strict, require, sha256_bytes

SOURCE_BYTES = 4 * 1024 * 1024
SOURCES = 256


def compatibility(request, configuration):
    """Conservative comparison identity; sources and private capture paths are provenance.

    The native matched timing verdict remains the improvement authority. This key only
    decides whether a prior report belongs to the declared diagnostic/workload series.
    """
    work = request["work"]
    return canonical_digest(dict(work=work, workload=configuration["diagnostic" if work == "profile" else work],
        environment=request["environment_digest"], producer=request["producer_digest"]))


def native(journal, ids, verifier=executor.verify_original):
    require(len(ids) <= SOURCES and len(set(ids)) == len(ids), "bounded unique original IDs required")
    records = []
    for ident in ids:
        request = journal.request(ident)
        state = journal.state(ident)
        result_digest = canonical_digest(state)
        outcome = "inconclusive"
        if state["state"] == "complete":
            request, state, result = verifier(journal, ident)
            result_digest = state["result_sha256"]
            outcome = {"pass": "positive", "fail": "negative"}.get(result["measurement_status"], "inconclusive")
        configuration = contracts.read_json(executor.read(os.path.join(journal.path(ident),
                                                     "configuration.json")), "executor")
        # Even terminal failures and uncertain executions remain history. Never discard a
        # negative result to make the next research packet look more promising.
        records.append(dict(kind="performance", source_id=canonical_digest(request),
            provenance="verified_native" if state["state"] == "complete" else "native_journal",
            freshness_basis="native_terminal" if state["ended_unix_ms"] is not None else "native_created",
            source_digest=result_digest, observed_unix_ms=state["ended_unix_ms"] or request["created_unix_ms"],
            compatibility=compatibility(request, configuration), compatibility_basis="native_request", outcome=outcome,
            summary="native %s experiment %s: %s; baseline %s; candidate %s; no standalone speedup claim" % (
                request["work"], ident, state["state"], request["sources"]["baseline_sha"],
                request["sources"]["candidate_sha"])))
    return records


def resolve(config, selection):
    """Retrieve one exact artifact via the trusted native reader and its authoritative Store."""
    require(set(selection) == {"task_id", "artifact_id", "observed_unix_ms", "compatibility"},
            "native evidence selection requires Task and artifact IDs, never file paths")
    require(os.path.isabs(config["af"]) and sha256_bytes(executor.read(config["af"], 256 * 1024 * 1024))
            == config["af_digest"], "installed evidence reader bytes changed")
    with tempfile.TemporaryDirectory() as home, tempfile.TemporaryFile() as output:
        result = subprocess.run([config["af"], "scenario", "evidence-source", selection["task_id"],
            "--artifact-id", selection["artifact_id"], "--repo", config["task_repo"],
            "--task-state", config["task_state"], "--json"], stdin=subprocess.DEVNULL, stdout=output,
            stderr=subprocess.DEVNULL, timeout=60,
            env={"PATH": os.defpath, "HOME": home, "XDG_CONFIG_HOME": home,
                 "XDG_STATE_HOME": home, "XDG_CACHE_HOME": home, "AF_SELF_OFFLINE": "1"})
        require(result.returncode == 0, "native evidence source refused; inspect its Task and artifact receipt")
        output.seek(0)
        data = output.read(SOURCE_BYTES + 1)
    return loads_strict(data, "native evidence source", SOURCE_BYTES)


def captured(sources, config=None, resolver=resolve):
    """Resolve bounded owner selections through native authority; never trust captured JSON."""
    require(isinstance(sources, list) and len(sources) <= SOURCES, "too many evidence sources")
    require(not sources or config is not None, "native evidence reader configuration required")
    out = []
    for selection in sources:
        require(isinstance(selection, dict) and set(selection) == {
            "task_id", "artifact_id", "observed_unix_ms", "compatibility"},
            "native evidence selection requires Task and artifact IDs, never file paths")
        expected, observed, compatible = (selection[k] for k in ("artifact_id", "observed_unix_ms", "compatibility"))
        freshness_basis = "capture_observed"
        contracts.DIGEST.validate(expected, "source artifact ID")
        contracts.DIGEST.validate(compatible, "compatibility")
        contracts.UNIX_MS.validate(observed, "source observation time")
        source = resolver(config, selection)
        require(source["schema"] == "af/scenario-evidence-source@1"
                and source["task_id"] == selection["task_id"]
                and source["artifact"]["artifact_id"] == expected, "native source receipt changed")
        envelope = source["artifact"]
        doc = envelope["payload"]
        receipt = dict(task_id=source["task_id"], revision_id=source["revision_id"], artifact_id=expected)
        schema = doc.get("schema")
        if schema == "af.task-review-findings/1":
            findings = doc["findings"]
            require(isinstance(findings, list) and len(findings) <= 512, "invalid findings capture")
            require(envelope["type"] == "af/TaskReviewFindings@1", "wrong native findings type")
            def bounded(value, limit):
                value = " ".join(str(value or "").split())
                return value if len(value) <= limit else value[:limit - 12] + " [truncated]"
            for finding in findings:
                require(len(out) < 4096, "selected evidence exceeds history index item bound; narrow selection")
                summary = "finding %s by %s: %s; at %s; evidence: %s; fix: %s" % (
                    bounded(finding["id"], 80), bounded(finding["by"], 80), bounded(finding["title"], 300),
                    bounded(finding.get("file"), 160), bounded(finding["body"], 850), bounded(finding.get("fix"), 350))
                out.append(dict(kind="finding", source_id=canonical_digest([expected, finding["id"]]),
                    source_digest=envelope["content_id"], provenance="native_task", native_receipt=receipt,
                    freshness_basis=freshness_basis, observed_unix_ms=observed, compatibility=compatible, compatibility_basis="owner_selection",
                    outcome="negative", summary=summary))
            if findings:
                continue
            outcome, kind = "inconclusive", "finding"
            summary = "no prior findings; %d missing reviewers" % len(doc["missing_reviewers"])
        elif schema == "af.optimization-history/1":
            require(envelope["type"] == "af/OptimizationHistory@1", "wrong native History Capture type")
            require(len(doc["observations"]) <= 100000 and len(doc["receipts"]) <= 200,
                    "History Capture exceeds native bounds")
            observed = int(doc["cutoff_unix_ms"])
            freshness_basis = "native_cutoff"
            outcome, kind = "diagnostic", "telemetry"
            summary = "%d normalized observations; %d source receipts; %d gaps; diagnostic only" % (
                len(doc["observations"]), len(doc["receipts"]), len(doc.get("gaps", [])))
            # Maxima keep overlapping spans distinct from wall time; absent/unknown samples
            # stay absent. Never sum cumulative token snapshots as independent spending.
            maxima, unknown = {}, 0
            for observation in doc["observations"]:
                for span in observation.get("spans", []):
                    if span.get("status") != "exact":
                        unknown += 1
                        continue
                    span_kind = span.get("kind")
                    if span_kind not in ("end_to_end", "active", "check", "tool", "retrieval", "verification"):
                        continue
                    start, end = span.get("start_unix_ms"), span.get("end_unix_ms")
                    require(all(isinstance(v, str) and v.isascii() and v.isdecimal() and len(v) <= 20
                                for v in (start, end)) and int(end) >= int(start), "invalid normalized span")
                    maxima[span_kind] = max(maxima.get(span_kind, 0), int(end) - int(start))
            summary += "; exact span maxima in ms: " + ", ".join("%s=%d" % p for p in sorted(maxima.items()))
            summary += "; %d non-exact spans excluded; maxima are not total or causal time" % unknown
        elif schema == "af.optimization-adoption-observation/1":
            require(envelope["type"] == "af/OptimizationAdoptionObservation@1", "wrong native adoption type")
            require(doc["causal_claim"] is False, "adoption evidence cannot claim causation")
            contracts.DIGEST.validate(doc["commit_snapshot_id"], "adopted Snapshot")
            outcome, kind = "adopted", "finding"
            observed = int(doc["observed_unix_ms"])
            freshness_basis = "native_cutoff"
            summary = "recorded local adoption observation at Snapshot " + doc["commit_snapshot_id"] + "; no causal claim"
        else:
            require(False, "unsupported evidence source; use native findings, History Capture or adoption")
        require(len(out) < 4096, "selected evidence exceeds history index item bound; narrow selection")
        out.append(dict(kind=kind, source_id=expected, source_digest=envelope["content_id"],
                        provenance="native_task", native_receipt=receipt, freshness_basis=freshness_basis,
                        observed_unix_ms=observed, compatibility=compatible, compatibility_basis="owner_selection", outcome=outcome, summary=summary))
    return out


def rebuild(records):
    require(len(records) <= 4096, "evidence history index exceeds bound")
    unique = {}
    for record in records:
        key = record["source_id"]
        require(key not in unique or unique[key] == record, "conflicting source evidence")
        unique[key] = record
    index = dict(schema="af.perf-evidence-index/2", artifacts=sorted(unique.values(),
                key=lambda r: (r["observed_unix_ms"], r["source_id"])))
    require(len(contracts.write_bytes(index, "evidence-index")) <= INDEX_BYTES,
            "evidence history index exceeds its byte bound")
    return index


def packet(index, cutoff, created, wanted, max_age_ms, max_items=32, max_bytes=32768):
    contracts.CONTRACTS["evidence-index"][2](index)
    require(cutoff <= created, "evidence cutoff cannot be in the future")
    result = dict(schema="af.perf-evidence-packet/2", cutoff_unix_ms=cutoff, created_unix_ms=created,
        max_age_ms=max_age_ms, compatibility=wanted, max_items=max_items, max_bytes=max_bytes,
        index_digest=canonical_digest(index), items=[], omissions=[], telemetry_is_diagnostic=True)
    contracts.write_bytes(result, "evidence-packet")
    omitted = Counter()
    for artifact in reversed(index["artifacts"]):
        observed = artifact["observed_unix_ms"]
        if observed > cutoff:
            omitted["future"] += 1
            continue
        if len(result["items"]) >= max_items:
            omitted["item_limit"] += 1
            continue
        entry = dict(artifact=artifact, fresh=created - observed <= max_age_ms,
                     compatible=artifact["compatibility"] == wanted)
        # Reserve room for all three omission counters, including their actual JSON keys.
        candidate = dict(result, items=result["items"] + [entry], omissions=[
            dict(reason=reason, count=4096) for reason in ("future", "item_limit", "byte_limit")])
        if len(document_bytes(candidate)) > max_bytes:
            omitted["byte_limit"] += 1
            continue
        result["items"].append(entry)
    result["omissions"] = [dict(reason=reason, count=count) for reason, count in sorted(omitted.items())]
    data = contracts.write_bytes(result, "evidence-packet")
    require(len(data) <= max_bytes, "evidence packet exceeds byte allowance")
    return result


def retain(root, doc, contract):
    """Content-addressed immutable packets/indexes; corruption never silently overwrites one."""
    executor.private_directory(root)
    data = contracts.write_bytes(doc, contract)
    require(len(data) <= INDEX_BYTES, "retained evidence document exceeds its byte bound")
    path = os.path.join(root, sha256_bytes(data)[7:] + ".json")
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except FileExistsError:
        require(executor.read(path, 4 * 1024 * 1024) == data, "immutable evidence packet changed")
        return path
    try:
        require(os.write(fd, data) == len(data), "short evidence write")
        os.fsync(fd)
        executor.sync_dir(root)
    finally:
        os.close(fd)
    return path
