"""Independent reference fixtures and golden oracle entries of the native probe (ADR-0163).

The native probe (`crates/review-pipeline/examples/perf-probe.rs`) generates its workload
fixtures through the product's own writers and reports what each timed operation produced.
This module never runs the product. It constructs, from the same fixture *inputs*, the bytes
the product must produce, with an encoder of its own, and derives every golden digest, count
and semantic projection from those reference bytes or from explicit semantic facts. Nothing
here is learned from an execution being judged.

The encoder is deliberately not a general RFC 8785 implementation: it admits only objects with
printable-ASCII keys, printable-ASCII strings that need no escape, booleans, null, and integers
inside the interoperable range. For such values UTF-16 key order equals byte order and number
formatting is the decimal integer, so the canonical spelling has exactly one independent
construction. Anything else is refused, so a fixture cannot drift into a region this oracle
does not cover.
"""
import hashlib
import operator
import re
import struct

from .values import ContractError, require

GENERATOR = "perflab.reference/1"
GOLDEN_SCHEMA = "af.perf-golden/2"

DOMAIN_CONTENT = b"review.kernel/content-id/v1\0"
DOMAIN_ARTIFACT = b"review.kernel/artifact-id/v1\0"
DOMAIN_MANIFEST = b"review.kernel/source-manifest/v1\0"

TASK_LEASE_MS = 15000
ENVELOPE_CAP_BYTES = 8 * 1024 * 1024
SAFE_INTEGER = 2 ** 53 - 1

# Fixture inputs. They mirror crates/review-pipeline/examples/perf-probe/fixtures.rs byte for
# byte; the probe's golden test fails when the two disagree, because the probe's envelopes would
# no longer carry these artifact IDs.
POLICY_DOCUMENT = {"perf_probe": "trusted fixture policy"}
FIXTURE_PRODUCER = {"kind": "kernel_operation", "operation_id": "fixture@1",
                    "run_id": "perf-probe"}
PACKAGE_PRODUCER = {"kind": "kernel_operation", "operation_id": "capture-package@1",
                    "run_id": "task-catalog-v1"}
REQUIREMENTS = "af/Requirements@1"
TASK_PACKAGE = "af/TaskPackage@1"
AUTHOR_PACKAGE = "perf/author"
CHECKER_PACKAGE = "perf/checker"
PIPELINE_PACKAGE = "perf/two-node"
PACKAGE_VERSION = "1.0.0"
PADDING_PATH = "data/padding.bin"


def _worker_toml(name, role, input_type, policy_id):
    return ('schema = "af.worker/1"\n'
            'name = "%s"\n'
            'version = "1.0.0"\n'
            '\n'
            '[signature]\n'
            'effects = []\n'
            'roles = ["%s"]\n'
            'worker_input_type = "%s"\n'
            'worker_output_type = "af/CheckedDocument@1"\n'
            'outcome_port = "output"\n'
            '\n'
            '[signature.contract.inputs.input]\n'
            'artifact_type = "%s"\n'
            'cardinality = "one"\n'
            'optional = false\n'
            'covers = []\n'
            '\n'
            '[signature.contract.inputs.input.affinity]\n'
            'kind = "unbound"\n'
            '\n'
            '[signature.contract.outputs.output]\n'
            'artifact_type = "af/CheckedDocument@1"\n'
            'cardinality = "one"\n'
            'optional = false\n'
            'covers = []\n'
            '\n'
            '[signature.contract.outputs.output.affinity]\n'
            'kind = "unbound"\n'
            '\n'
            '[signature.evidence]\n'
            'output = ["%s"]\n'
            '\n'
            '[signature.retains]\n'
            '\n'
            '[signature.attempt]\n'
            'tokens = 0\n'
            'wall_ms = 5000\n'
            '\n'
            '[runner]\n'
            'kind = "command"\n'
            '\n'
            '[runner.command]\n'
            'program = "/usr/bin/false"\n'
            'args = []\n') % (name, role, input_type, input_type, policy_id)


INPUT_SCHEMA = ('{"type":"object","additionalProperties":false,"required":["input"],'
                '"properties":{"input":{"type":"array","minItems":1,"maxItems":1,"items":'
                '{"type":"object","additionalProperties":false,"required":["artifact_id",'
                '"artifact_type","payload"],"properties":{"artifact_id":{"type":"string"},'
                '"artifact_type":{"type":"string"},"payload":{"type":"object"}}}}}}')
OUTPUT_SCHEMA = ('{"type":"object","additionalProperties":false,"required":["outcome","text"],'
                 '"properties":{"outcome":{"enum":["passed","failed","inconclusive"]},'
                 '"text":{"type":"string"}}}')
INSTRUCTIONS = "Performance fixture Worker. It is never executed.\n"

PIPELINE_TOML = """schema = "af.pipeline/1"
name = "perf/two-node"
version = "1.0.0"
max_attempts = 3
max_parallel = 1

[contract.inputs.requirements]
artifact_type = "af/Requirements@1"
cardinality = "one"
optional = false
covers = []

[contract.inputs.requirements.affinity]
kind = "unbound"

[contract.outputs.document]
artifact_type = "af/CheckedDocument@1"
cardinality = "one"
optional = false
covers = ["checked"]

[contract.outputs.document.affinity]
kind = "unbound"

[accepts]
kinds = ["document"]

[accepts.required_facts]

[slots.author]
worker = "perf/author"
role = "author"
input_type = "af/Requirements@1"
output_type = "af/CheckedDocument@1"
min_attempts = 1
max_attempts = 1
allow_local_replacement = false
independent_from = []

[slots.checker]
worker = "perf/checker"
role = "check"
input_type = "af/CheckedDocument@1"
output_type = "af/CheckedDocument@1"
min_attempts = 1
max_attempts = 1
allow_local_replacement = false
independent_from = []

[[nodes]]
id = "draft"

[nodes.operator]
op = "worker"
slot = "author"

[nodes.inputs.input]
kind = "input"
port = "requirements"

[[nodes]]
id = "check"

[nodes.operator]
op = "worker"
slot = "checker"

[nodes.inputs.input]
kind = "node"
node = "draft"
port = "output"

[outputs.document]
kind = "node"
node = "check"
port = "output"

[coverage.checked]
kind = "node"
node = "check"
port = "output"
"""

SENTINEL_VALUE = {
    "kind": "af-perf-sentinel",
    "items": [index * 977 for index in range(64)],
    "label": "baseline versus baseline health workload",
    "nested": {"b": [True, False, None], "a": 1},
}
SENTINEL_REPETITIONS = 64
SOURCE_FILE_BYTES = 128
SOURCE_TREES = (100, 10000)
PERSISTENCE_BLOB_BYTES = 1024 * 1024
PERSISTENCE_EVENTS = 100
# Open, propose and admit, then the production lease refresh immediately before the region.
PERSISTENCE_PREFIX_EVENTS = 4
PROJECTION_EVENTS = 10000
TASK_ID = "perf-probe-task"
GRAPH_ORDER = ["root.inputs", "root.nodes.draft", "root.nodes.check"]
GRAPH_SLOTS = {"root.slots.author": AUTHOR_PACKAGE, "root.slots.checker": CHECKER_PACKAGE}
# How each registry typed envelope is generated, as the probe's ENVELOPES table declares.
ENVELOPE_GENERATORS = {"small_envelope": "requirements_text",
                       "issue_255_medium": "author_package_padding",
                       "issue_255_large": "author_package_padding"}


# The restricted canonical encoder -----------------------------------------------------------

# A C-level scan of exactly the same domain as the encoder: printable ASCII, excluding
# double quotes and backslashes. This also refuses DEL, non-ASCII and lone surrogates.
_UNSUPPORTED_STRING = re.compile(r'[^\x20-\x21\x23-\x5b\x5d-\x7e]')


def _string(text, where):
    require(isinstance(text, str), where + ": expected a string")
    require(_UNSUPPORTED_STRING.search(text) is None,
            where + ": the reference encoder admits only printable ASCII without escapes")
    return '"' + text + '"'


def _encode(value, out, where):
    if value is None:
        out.append("null")
    elif value is True:
        out.append("true")
    elif value is False:
        out.append("false")
    elif type(value) is int:
        require(-SAFE_INTEGER <= value <= SAFE_INTEGER, where + ": outside the safe integers")
        out.append("%d" % value)
    elif isinstance(value, str):
        out.append(_string(value, where))
    elif isinstance(value, (list, tuple)):
        out.append("[")
        for index, item in enumerate(value):
            if index:
                out.append(",")
            _encode(item, out, "%s[%d]" % (where, index))
        out.append("]")
    elif isinstance(value, dict):
        keys = list(value)
        for key in keys:
            _string(key, where + ": key")
        # Printable ASCII: UTF-16 code-unit order is byte order.
        out.append("{")
        for index, key in enumerate(sorted(keys)):
            if index:
                out.append(",")
            out.append(_string(key, where))
            out.append(":")
            _encode(value[key], out, where + "." + key)
        out.append("}")
    else:
        raise ContractError(where + ": the reference encoder refuses %s"
                            % type(value).__name__)


def canonical(value, where="reference"):
    out = []
    _encode(value, out, where)
    return "".join(out).encode("ascii")


def _bytes_array(data):
    """The canonical spelling of a byte vector serialized as a JSON array of integers."""
    return "[" + ",".join("%d" % byte for byte in data) + "]"


def content_id(data):
    return "sha256:" + hashlib.sha256(DOMAIN_CONTENT + data).hexdigest()


def artifact_digest(data):
    return "sha256:" + hashlib.sha256(DOMAIN_ARTIFACT + data).hexdigest()


# Envelopes ----------------------------------------------------------------------------------

class Envelope:
    """Reference canonical bytes and identities of one typed envelope."""

    def __init__(self, artifact_type, producer, payload_bytes):
        self.artifact_type = artifact_type
        self.content_id = content_id(payload_bytes)
        head = {"content_id": self.content_id, "input_artifacts": [], "producer": producer,
                "type": artifact_type}
        # Splice the payload, which the encoder may not see whole (byte arrays), between keys.
        without_id = self._join(head, payload_bytes, None)
        self.artifact_id = artifact_digest(without_id)
        self.bytes = self._join(head, payload_bytes, self.artifact_id)
        self.blob_id = content_id(self.bytes)

    @staticmethod
    def _join(head, payload_bytes, artifact_id):
        fields = dict(head)
        if artifact_id is not None:
            fields["artifact_id"] = artifact_id
        fields["payload"] = None
        parts = []
        for index, key in enumerate(sorted(fields)):
            parts.append(("," if index else "") + _string(key, "envelope") + ":")
            if key == "payload":
                parts.append(payload_bytes.decode("ascii"))
            else:
                parts.append(canonical(fields[key], "envelope." + key).decode("ascii"))
        return ("{" + "".join(parts) + "}").encode("ascii")

    def projection(self):
        """The semantic projection the validate and typed-get cases report."""
        return {"artifact_id": self.artifact_id, "artifact_type": self.artifact_type,
                "content_id": self.content_id, "envelope_bytes": len(self.bytes),
                "input_artifacts": 0, "producer_kind": "kernel_operation"}


def _periodic(pattern, count):
    """Construct a deterministic prefix without a Python iteration per character/byte.

    Like range(count), accept integer indices and make a negative count empty.
    """
    repeats, remainder = divmod(max(0, operator.index(count)), len(pattern))
    return pattern * repeats + pattern[:remainder]


def letters(count):
    return _periodic("abcdefghijklmnopqrstuvwxyz", count)


def requirements_envelope(text_bytes):
    payload = canonical({"text": letters(text_bytes)}, "requirements")
    return Envelope(REQUIREMENTS, FIXTURE_PRODUCER, payload)


def policy_id():
    return content_id(canonical(POLICY_DOCUMENT, "policy"))


def padding(digits, tens):
    return _periodic(b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09", digits) + bytes([10]) * tens


def author_files(padding_bytes):
    policy = policy_id()
    files = {
        "worker.toml": _worker_toml(AUTHOR_PACKAGE, "author", REQUIREMENTS, policy).encode(),
        "input.schema.json": INPUT_SCHEMA.encode(),
        "outputs/output.schema.json": OUTPUT_SCHEMA.encode(),
        "instructions.md": INSTRUCTIONS.encode(),
    }
    if padding_bytes is not None:
        files[PADDING_PATH] = padding_bytes
    return files


def checker_files():
    policy = policy_id()
    return {
        "worker.toml": _worker_toml(CHECKER_PACKAGE, "check", "af/CheckedDocument@1",
                                    policy).encode(),
        "input.schema.json": INPUT_SCHEMA.encode(),
        "outputs/output.schema.json": OUTPUT_SCHEMA.encode(),
        "instructions.md": INSTRUCTIONS.encode(),
    }


def pipeline_files():
    return {"pipeline.toml": PIPELINE_TOML.encode()}


def package_digest(files):
    listed = dict((path, content_id(data)) for path, data in files.items())
    return content_id(canonical({"files": listed, "kind": "reviewer-package@1"}, "package"))


def package_envelope(name, files):
    """The af/TaskPackage@1 envelope the compiler's capture writes for `files`."""
    for path in files:
        _string(path, "package path")
    spelled = ",".join(_string(path, "path") + ":" + _bytes_array(files[path])
                       for path in sorted(files))
    head = {"digest": package_digest(files), "name": name, "schema": "af.task-package/1",
            "version": PACKAGE_VERSION}
    # Keys: digest, files, name, schema, version.
    payload = ("{" + _string("digest", "k") + ":" + _string(head["digest"], "v") + ","
               + _string("files", "k") + ":{" + spelled + "},"
               + _string("name", "k") + ":" + _string(name, "v") + ","
               + _string("schema", "k") + ":" + _string(head["schema"], "v") + ","
               + _string("version", "k") + ":" + _string(PACKAGE_VERSION, "v") + "}")
    return Envelope(TASK_PACKAGE, PACKAGE_PRODUCER, payload.encode("ascii"))


def solve_padding(target):
    """Padding (digits, tens) that makes the author package envelope exactly `target` bytes.

    One digit byte spells `[d]`; each further digit byte adds `,d` (two bytes) and each ten
    `,10` (three bytes), so every size from the one-digit base upward is reachable.
    """
    base = len(package_envelope(AUTHOR_PACKAGE, author_files(padding(1, 0))).bytes)
    gap = target - base
    require(gap >= 0, "envelope target %d is below the package base %d" % (target, base))
    tens = gap % 2
    digits = 1 + (gap - 3 * tens) // 2
    require(digits >= 1, "envelope target %d is unreachable" % target)
    return digits, tens


def solve_requirements(target):
    base = len(requirements_envelope(0).bytes)
    require(target >= base, "envelope target %d is below the base %d" % (target, base))
    return target - base


# Source trees -------------------------------------------------------------------------------

def source_file(index):
    path = "src/d%03d/f%05d.txt" % (index // 100, index)
    data = ("af performance source fixture file %05d\n" % index).encode("ascii")
    alphabet = b"abcdefghijklmnopqrstuvwxyz"
    offset = index % len(alphabet)
    data += _periodic(alphabet[offset:] + alphabet[:offset], SOURCE_FILE_BYTES - 1 - len(data))
    return path, data + b"\n"


def _framed(field):
    return struct.pack(">Q", len(field)) + field


def manifest_digest(files):
    """review.kernel/source-manifest/v1 over (path, mode, content ID, size), path-byte order."""
    entries = sorted((path.encode("ascii"), data) for path, data in files)
    hasher = hashlib.sha256(DOMAIN_MANIFEST)
    hasher.update(struct.pack(">Q", len(entries)))
    for path, data in entries:
        for field in (path, b"100644", content_id(data).encode("ascii")):
            hasher.update(_framed(field))
        hasher.update(struct.pack(">Q", len(data)))
    return "sha256:" + hasher.hexdigest()


def persistence_blob():
    return bytes((index * 31 + 7) % 251 for index in range(PERSISTENCE_BLOB_BYTES))


def persistence_logical_bytes(operation):
    """The independent logical bytes of a persistence variant, or None when they are not fixed.

    The CAS round trip writes and reads back the reference object: exactly twice its size. The
    state round trip's bytes are the canonical appended events, whose timestamps and
    identifiers vary per run; they are reported and checked for shape, never compared with a
    golden value."""
    if operation == "cas_write_read":
        return 2 * PERSISTENCE_BLOB_BYTES
    return None


# Golden entries -----------------------------------------------------------------------------

def _digest_of(semantic):
    return content_id(canonical(semantic, "semantic projection"))


def _envelope_fixture(fixture_id, fixture):
    require(fixture["kind"] == "typed_envelope",
            "fixtures.%s: not a typed envelope" % fixture_id)
    size = fixture["serialized_envelope_bytes"]
    require(fixture_id in ENVELOPE_GENERATORS,
            "fixtures.%s: no reference generator" % fixture_id)
    if ENVELOPE_GENERATORS[fixture_id] == "requirements_text":
        text_bytes = solve_requirements(size)
        envelope = requirements_envelope(text_bytes)
        generation = {"kind": "requirements_text", "text_bytes": text_bytes}
    else:
        digits, tens = solve_padding(size)
        envelope = package_envelope(AUTHOR_PACKAGE, author_files(padding(digits, tens)))
        generation = {"kind": "author_package_padding", "padding_digits": digits,
                      "padding_tens": tens}
    # Generation reaches every declared size exactly, approximate or not.
    require(len(envelope.bytes) == size, "fixtures.%s: generated %d bytes, not %d"
            % (fixture_id, len(envelope.bytes), size))
    return envelope, generation


def _source_files(count):
    return [source_file(index) for index in range(count)]


def _authority_semantic(author):
    packages = {}
    for name, envelope in ((AUTHOR_PACKAGE, author),
                           (CHECKER_PACKAGE, package_envelope(CHECKER_PACKAGE, checker_files())),
                           (PIPELINE_PACKAGE, package_envelope(PIPELINE_PACKAGE,
                                                               pipeline_files()))):
        packages[name] = {"artifact_id": envelope.artifact_id, "content_id": envelope.content_id,
                          "envelope_bytes": len(envelope.bytes)}
    return {"admitted": True, "graph_order": GRAPH_ORDER, "graph_slots": GRAPH_SLOTS,
            "packages": packages, "phase": {"kind": "running"}, "plan_recorded": True,
            "task_id": TASK_ID}


def _entry(instance, contract_id, fixture_digest, digest, count, semantic, basis):
    of_semantic = semantic is not None and digest == _digest_of(semantic)
    return {"basis": basis, "case_id": instance["case_id"], "contract_id": contract_id,
            "count": count, "digest": digest,
            "digest_of": "semantic" if of_semantic else "reference_bytes",
            "fixture_digest": fixture_digest, "instance_id": instance["instance_id"],
            "parameters": instance["parameters"], "semantic": semantic}


def golden_entry(doc, instance, contract_id, cache):
    """The golden entry of one expanded instance, from reference bytes and semantic facts."""
    fixtures = doc["fixtures"]
    params = instance["parameters"]

    def envelope():
        key = params["package"]
        if key not in cache:
            cache[key] = _envelope_fixture(key, fixtures[key])
        return cache[key][0]

    if contract_id == "canonical_serialize_v1":
        env = envelope()
        return _entry(instance, contract_id, env.artifact_id, env.blob_id, len(env.bytes), None,
                      "content ID and length of the reference canonical envelope bytes")
    if contract_id in ("canonical_validate_v1", "cas_typed_get_validate_v1"):
        env = envelope()
        semantic = env.projection()
        return _entry(instance, contract_id, env.artifact_id, _digest_of(semantic),
                      len(env.bytes), semantic,
                      "semantic projection of the reference envelope; count is its full size")
    if contract_id == "cas_raw_get_verify_v1":
        env = envelope()
        return _entry(instance, contract_id, env.artifact_id, env.blob_id, len(env.bytes), None,
                      "the reference envelope bytes filed as one raw object")
    if contract_id in ("source_capture_v2", "source_capture_seed_v1"):
        files = _source_files(params["files"])
        semantic = {"entries": len(files), "file_bytes": SOURCE_FILE_BYTES,
                    "first_path": files[0][0], "last_path": files[-1][0], "mode": "100644"}
        return _entry(instance, contract_id, None, manifest_digest(files), len(files), semantic,
                      "source-manifest digest of the reference paths and contents" + (
                          ", as the seed writer reads its written tree back"
                          if contract_id == "source_capture_seed_v1" else ""))
    if contract_id in ("state_projection_v2", "state_projection_seed_v1"):
        events = fixtures[params["log"]]["events"]
        semantic = {"admitted": True, "events": events, "next_sequence": events,
                    "phase": {"kind": "running"}, "plan_recorded": True, "task_id": TASK_ID}
        return _entry(instance, contract_id, None, _digest_of(semantic), events, semantic,
                      "explicit final-state facts of exactly %d relevant events" % events)
    if contract_id in ("authority_plan_check_v3", "authority_runtime_new_v3", "authority_seed_v1"):
        env = envelope()
        semantic = _authority_semantic(env)
        return _entry(instance, contract_id, env.artifact_id, _digest_of(semantic),
                      len(env.bytes), semantic,
                      "graph invariants and reference identities of every captured package")
    if contract_id == "persistence_fs_v1":
        if params["operation"] == "cas_write_read":
            blob = content_id(persistence_blob())
            return _entry(instance, contract_id, blob, blob,
                          persistence_logical_bytes("cas_write_read"), None,
                          "the reference object read back; count is logical bytes written+read")
        semantic = {"appended": PERSISTENCE_EVENTS, "event_types": ["TaskTransition@5"],
                    "prefix_events": PERSISTENCE_PREFIX_EVENTS}
        return _entry(instance, contract_id, None, _digest_of(semantic), PERSISTENCE_EVENTS,
                      semantic, "explicit facts of the appended and read-back lease renewals")
    if contract_id == "sentinel_canonical_v1":
        once = canonical(SENTINEL_VALUE, "sentinel")
        return _entry(instance, contract_id, None, content_id(once * SENTINEL_REPETITIONS),
                      SENTINEL_REPETITIONS, None,
                      "content ID of every repetition's reference canonical bytes, in order")
    raise ContractError("no reference for contract " + contract_id)


def native_instances(doc):
    """Every expanded instance, across tiers, of a library case whose native oracle is
    available, plus the health sentinel, each with its probe contract and repetitions."""
    from . import suite as suite_module
    seen = {}
    for tier in sorted(doc["tiers"]):
        for item in suite_module.expand(doc, tier):
            case = doc["cases"][item["case_id"]]
            if case["kind"] != "library" or case["oracle"]["state"] != "available":
                continue
            seen[item["instance_id"]] = (item, case["probe"]["contract_id"],
                                         case["probe"]["repetitions"])
    health = doc["health"]
    if health["oracle"]["state"] == "available":
        sentinel = {"instance_id": health["workload"], "case_id": health["workload"],
                    "parameters": {}}
        seen[health["workload"]] = (sentinel, doc["probe"]["sentinel_contract"],
                                    health["batch_repetitions"])
    return [seen[key] for key in sorted(seen)]


def native_seeds(doc):
    """Every seed (ADR-0170) a native instance of any tier reads, once, by seed ID: the writer
    request's identity as `{"instance_id", "case_id", "parameters"}` and its writer contract."""
    from . import suite as suite_module
    seen = {}
    for instance, _, _ in native_instances(doc):
        if instance["case_id"] == doc["health"]["workload"]:
            continue
        seed = suite_module.seed_of(doc, instance)
        if seed is not None:
            seen[seed["seed_id"]] = ({"instance_id": seed["seed_id"], "case_id": seed["case_id"],
                                      "parameters": seed["parameters"]}, seed["contract_id"])
    return [seen[key] for key in sorted(seen)]


def golden_document(doc):
    """The complete af.perf-golden/2 document for a validated registry document: an entry per
    native instance and, for every seed one reads, the entry its writer's check must match."""
    cache = {}
    entries = []
    for instance, contract_id, _ in native_instances(doc):
        entries.append(golden_entry(doc, instance, contract_id, cache))
    seeds = [golden_entry(doc, writer, contract_id, cache)
             for writer, contract_id in native_seeds(doc)]
    fixtures = {}
    for fixture_id, fixture in sorted(doc["fixtures"].items()):
        if fixture["golden"] != "available":
            continue
        if fixture["kind"] == "typed_envelope":
            if fixture_id not in cache:
                cache[fixture_id] = _envelope_fixture(fixture_id, fixture)
            envelope, generation = cache[fixture_id]
            fixtures[fixture_id] = {
                "kind": "typed_envelope", "artifact_type": envelope.artifact_type,
                "serialized_envelope_bytes": len(envelope.bytes), "generation": generation,
                "artifact_id": envelope.artifact_id, "content_id": envelope.content_id,
                "envelope_blob_id": envelope.blob_id}
        elif fixture["kind"] == "canonical_small":
            once = canonical(SENTINEL_VALUE, "sentinel")
            fixtures[fixture_id] = {"kind": "canonical_small", "canonical": once.decode("ascii"),
                                    "content_id": content_id(once),
                                    "repetitions": doc["health"]["batch_repetitions"]}
        elif fixture["kind"] == "file_tree":
            trees = {}
            for count in SOURCE_TREES:
                trees[str(count)] = {"files": count,
                                     "manifest_digest": manifest_digest(_source_files(count))}
            fixtures[fixture_id] = {"kind": "file_tree", "file_bytes": fixture["file_bytes"],
                                    "trees": trees}
        elif fixture["kind"] == "event_log":
            fixtures[fixture_id] = {
                "kind": "event_log", "events": fixture["events"], "prefix_events": 3,
                "relevant_events": "TaskTransition@5 opened, plan proposed, plan admitted, then "
                                   "production 15000 ms lease renewals"}
        else:
            raise ContractError("fixtures.%s: no reference for kind %s"
                                % (fixture_id, fixture["kind"]))
    cap_text = solve_requirements(ENVELOPE_CAP_BYTES)
    admitted = requirements_envelope(cap_text)
    require(len(admitted.bytes) == ENVELOPE_CAP_BYTES, "the cap envelope is not exactly the cap")
    return {
        "schema": GOLDEN_SCHEMA, "generator": GENERATOR,
        "note": "Generated by perflab.reference from reference bytes and explicit semantic facts; "
                "regenerate with `python3 scripts/performance/cli.py golden --write`.",
        "constants": {"task_lease_ms": TASK_LEASE_MS,
                      "artifact_envelope_cap_bytes": ENVELOPE_CAP_BYTES,
                      "policy_id": policy_id()},
        "fixtures": fixtures,
        "cap_boundary": {
            "admitted": {"serialized_envelope_bytes": ENVELOPE_CAP_BYTES,
                         "text_bytes": cap_text, "artifact_id": admitted.artifact_id},
            "refused": {"serialized_envelope_bytes": ENVELOPE_CAP_BYTES + 1,
                        "text_bytes": cap_text + 1}},
        "entries": entries,
        "seeds": seeds,
    }
