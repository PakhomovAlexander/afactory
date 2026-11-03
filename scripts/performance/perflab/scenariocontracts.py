"""Closed contracts for asynchronous Scenario submissions and bounded research packets."""
from .spec import (Arr, Bool, Const, DIGEST, Field, Int, Nullable, Obj, Str)
from .values import document_bytes, require

CAPTURE_RESERVE = 12 * 1024 ** 3
MAX_REQUEST = 131072
INDEX_BYTES = 4 * 1024 * 1024


def register(c):
    text = Str(min_len=1, max_len=4096)
    path = Str(pattern=r"^/[^\x00\n]{1,1023}$", max_len=1024)
    source = Obj(Field("schema", Const("af.perf-scenario-experiment/1")),
                 Field("work", Str(enum=("core", "telemetry"))),
                 Field("configuration_digest", DIGEST), Field("producer_digest", DIGEST))
    config = Obj(Field("schema", Const("af.perf-scenario-adapter/1")),
                 Field("root", path), Field("executor", path),
                 Field("af", path), Field("af_digest", DIGEST),
                 Field("task_repo", path), Field("task_state", path),
                 Field("evidence_index", path), Field("evidence_index_digest", DIGEST),
                 Field("evidence_root", path), Field("evidence_max_age_ms", Int(0, 31536000000)),
                 Field("evidence_max_items", Int(1, 256)), Field("evidence_max_bytes", Int(1024, 24576)),
                 Field("destination", text), Field("max_intents", Int(1, 4096)),
                 Field("max_bytes", Int(CAPTURE_RESERVE + MAX_REQUEST, 2 ** 50)))
    intent = Obj(Field("schema", Const("af.perf-scenario-intent/1")),
                 Field("intent_id", DIGEST), Field("input_digest", DIGEST),
                 Field("destination", text), Field("subject", DIGEST), Field("revision", DIGEST),
                 Field("source_request_digest", DIGEST),
                 Field("created_unix_ms", c.UNIX_MS), Field("experiment", source))
    receipt = Obj(Field("schema", Const("af.perf-scenario-receipt/1")),
                  Field("intent_digest", DIGEST), Field("request_id", c.ID),
                  Field("request_digest", DIGEST))
    verification = Obj(Field("schema", Const("af.perf-scenario-verification/1")),
                       Field("receipt_digest", DIGEST), Field("result_sha256", DIGEST),
                       Field("verified_unix_ms", c.UNIX_MS),
                       Field("measurement_status", Str(enum=("pass", "fail", "incomplete", "complete", "partial", "failed"))),
                       Field("completion", Obj(Field("schema", Const("af.perf-scenario-completion/1")),
                           Field("experiment", source),
                           Field("request", c.CONTRACTS["executor-request"][1]),
                           Field("result_sha256", DIGEST), Field("source_request_digest", DIGEST),
                           Field("baseline_snapshot_id", DIGEST), Field("candidate_snapshot_id", DIGEST),
                           Field("verdict", Nullable(c.CONTRACTS["verdict"][1])),
                           Field("telemetry_is_diagnostic", Const(True)))))
    status = Obj(Field("schema", Const("af.perf-scenario-status/1")), Field("intent_digest", DIGEST),
                 Field("state", Str(enum=("refused", "uncertain"))), Field("detail", text))
    artifact = Obj(Field("kind", Str(enum=("finding", "performance", "telemetry"))),
                   Field("source_id", DIGEST), Field("source_digest", DIGEST),
                   Field("provenance", Str(enum=("verified_native", "native_journal", "native_task"))),
                   Field("freshness_basis", Str(enum=("native_terminal", "native_created", "native_cutoff", "capture_observed"))),
                   Field("observed_unix_ms", c.UNIX_MS), Field("compatibility", DIGEST),
                   Field("compatibility_basis", Str(enum=("native_request", "owner_selection"))),
                   Field("outcome", Str(enum=("positive", "negative", "inconclusive", "adopted", "diagnostic"))),
                   Field("summary", Str(min_len=1, max_len=2048)),
                   Field("native_receipt", Obj(Field("task_id", Str(min_len=1, max_len=128)),
                       Field("revision_id", DIGEST), Field("artifact_id", DIGEST)), required=False))
    entry = Obj(Field("artifact", artifact), Field("fresh", Bool()), Field("compatible", Bool()))
    packet = Obj(Field("schema", Const("af.perf-evidence-packet/2")),
                 Field("cutoff_unix_ms", c.UNIX_MS), Field("created_unix_ms", c.UNIX_MS),
                 Field("max_age_ms", Int(0, 31536000000)), Field("compatibility", DIGEST),
                 Field("max_items", Int(1, 256)), Field("max_bytes", Int(1024, 65536)),
                 Field("index_digest", DIGEST), Field("items", Arr(entry, 0, 256)),
                 Field("omissions", Arr(Obj(Field("reason", Str(enum=("future", "item_limit", "byte_limit"))),
                                             Field("count", Int(1, 4096))), 0, 3)),
                 Field("telemetry_is_diagnostic", Const(True)))
    index = Obj(Field("schema", Const("af.perf-evidence-index/2")),
                Field("artifacts", Arr(artifact, 0, 4096)))
    service = Obj(Field("schema", Const("af.perf-scenario-service/1")), Field("status", text))
    launcher = Obj(Field("schema", Const("af.perf-scenario-launcher/1")),
                   Field("executable", path), Field("digest", DIGEST))
    exported = Obj(Field("schema", Const("af.perf-evidence-export/1")),
                   Field("index", path), Field("index_digest", DIGEST), Field("packet", path))

    completion = verification.by_name["completion"].node

    def valid_completion(doc, where="scenario completion"):
        completion.validate(doc, where)
        native = doc["request"]
        c.CONTRACTS["executor-request"][2](native)
        require(native["backend"] == "local" and native["submission"]["kind"] == "manual"
                and native["work"] == doc["experiment"]["work"]
                and native["producer_digest"] == doc["experiment"]["producer_digest"],
                "completion differs from its declared native experiment")
        require((native["work"] == "core") == (doc["verdict"] is not None),
                "only core completion carries matched timing verdict evidence")
        if doc["verdict"] is not None:
            c.CONTRACTS["verdict"][2](doc["verdict"])
            require(doc["verdict"]["run_id"] == "local_" + native["request_id"][4:],
                    "completion verdict names another native original")
        require(len(document_bytes(doc)) <= 24576, "completion exceeds bounded Scenario evidence")
        return doc

    def valid_verification(doc, where="scenario verification"):
        verification.validate(doc, where)
        valid_completion(doc["completion"])
        require(doc["result_sha256"] == doc["completion"]["result_sha256"], "verification result digest changed")
        if doc["completion"]["verdict"] is not None:
            require(doc["measurement_status"] == doc["completion"]["verdict"]["aggregate"]["result"],
                    "verification status differs from native timing verdict")
        return doc

    def valid_index(doc, where="evidence index"):
        index.validate(doc, where)
        records = doc["artifacts"]
        for record in records:
            require((record["provenance"] == "native_task") == ("native_receipt" in record),
                    "native Task evidence requires its source receipt")
        require(len({r["source_id"] for r in records}) == len(records)
                and records == sorted(records, key=lambda r: (r["observed_unix_ms"], r["source_id"])),
                "evidence index requires unique sources in canonical history order")
        require(len(document_bytes(doc)) <= INDEX_BYTES, "evidence index exceeds its byte bound")
        return doc

    def valid_packet(doc, where="evidence packet"):
        packet.validate(doc, where)
        require(doc["cutoff_unix_ms"] <= doc["created_unix_ms"] and len(doc["items"]) <= doc["max_items"]
                and len(document_bytes(doc)) <= doc["max_bytes"], "packet exceeds its cutoff or limits")
        ids = [item["artifact"]["source_id"] for item in doc["items"]]
        require(len(set(ids)) == len(ids), "packet contains duplicate sources")
        reasons = [item["reason"] for item in doc["omissions"]]
        require(len(set(reasons)) == len(reasons)
                and len(ids) + sum(item["count"] for item in doc["omissions"]) <= 4096,
                "packet omission accounting exceeds the bounded index")
        for item in doc["items"]:
            source = item["artifact"]
            require((source["provenance"] == "native_task") == ("native_receipt" in source),
                    "native Task evidence requires its source receipt")
            require(source["observed_unix_ms"] <= doc["cutoff_unix_ms"]
                    and item["fresh"] == (doc["created_unix_ms"] - source["observed_unix_ms"] <= doc["max_age_ms"])
                    and item["compatible"] == (source["compatibility"] == doc["compatibility"]),
                    "packet freshness, compatibility or cutoff is stale")
        return doc

    for name, node, validator in (
            ("scenario-experiment", source, None), ("scenario-adapter", config, None),
            ("scenario-intent", intent, None), ("scenario-receipt", receipt, None),
            ("scenario-verification", verification, valid_verification), ("evidence-index", index, valid_index),
            ("scenario-completion", completion, valid_completion),
            ("evidence-packet", packet, valid_packet), ("scenario-status", status, None),
            ("scenario-service", service, None), ("scenario-launcher", launcher, None),
            ("evidence-export", exported, None)):
        c.CONTRACTS[name] = (node.by_name["schema"].node.value, node,
                             validator or (lambda doc, where="scenario", n=node: n.validate(doc, where)),
                             "Native Scenario lab integration.")
