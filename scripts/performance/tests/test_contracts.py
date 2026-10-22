"""Schema parity: committed JSON Schemas, golden fixtures and the readers agree.

Every golden fixture under fixtures/performance/schema is read by its contract's reader. An
independent evaluator of the JSON Schema subset the generator emits then checks the committed
schema files: valid fixtures pass, shape-invalid fixtures fail, and semantic-invalid fixtures
pass the schema, which proves the reader enforces what a schema cannot state.
"""
import json
import os
import re
import unittest

from perflab import adapters, clicases, contracts, diagnostics, suite, tomlfile
from perflab.values import ContractError
from tests.support import ROOT, read

FIXTURES = os.path.join(ROOT, "fixtures/performance/schema")
SCHEMAS = os.path.join(ROOT, "schemas")
TOML_CONTRACTS = ("suite", "activation", "adapters", "cli", "diagnostics")


class SchemaSubset:
    """Evaluate the JSON Schema 2020-12 keywords the generator emits; refuse any other."""

    KNOWN = {"$schema", "$id", "title", "description", "$defs", "$ref", "type", "properties",
             "required", "additionalProperties", "patternProperties", "minProperties",
             "maxProperties", "items", "minItems", "maxItems", "uniqueItems", "enum", "const",
             "pattern", "minLength", "maxLength", "minimum", "maximum", "anyOf", "oneOf"}

    def __init__(self, root):
        self.root = root

    def valid(self, value, schema=None):
        return self.check(value, self.root if schema is None else schema)

    def check(self, value, schema):
        unknown = set(schema) - self.KNOWN
        if unknown:
            raise AssertionError("unsupported schema keywords %s" % sorted(unknown))
        if "$ref" in schema:
            name = schema["$ref"].split("/")[-1]
            return self.check(value, self.root["$defs"][name])
        if "anyOf" in schema and not any(self.check(value, s) for s in schema["anyOf"]):
            return False
        if "oneOf" in schema and sum(1 for s in schema["oneOf"] if self.check(value, s)) != 1:
            return False
        if "const" in schema and not (type(value) is type(schema["const"])
                                      and value == schema["const"]):
            return False
        if "enum" in schema and value not in schema["enum"]:
            return False
        kind = schema.get("type")
        if kind == "object":
            return isinstance(value, dict) and self.object(value, schema)
        if kind == "array":
            return isinstance(value, list) and self.array(value, schema)
        if kind == "string":
            return isinstance(value, str) and self.string(value, schema)
        if kind == "integer":
            return (type(value) is int and schema.get("minimum", value) <= value
                    <= schema.get("maximum", value))
        if kind == "boolean":
            return type(value) is bool
        if kind == "null":
            return value is None
        return True

    def object(self, value, schema):
        props = schema.get("properties", {})
        patterns = schema.get("patternProperties", {})
        if any(key not in value for key in schema.get("required", [])):
            return False
        if not schema.get("minProperties", 0) <= len(value) <= schema.get("maxProperties",
                                                                          len(value)):
            return False
        for key, item in value.items():
            if key in props:
                if not self.check(item, props[key]):
                    return False
                continue
            matched = [s for p, s in patterns.items() if re.search(p, key)]
            if not matched and schema.get("additionalProperties", True) is False:
                return False
            if not all(self.check(item, s) for s in matched):
                return False
        return True

    def array(self, value, schema):
        if not schema.get("minItems", 0) <= len(value) <= schema.get("maxItems", len(value)):
            return False
        if schema.get("uniqueItems"):
            dumped = [json.dumps(item, sort_keys=True) for item in value]
            if len(set(dumped)) != len(dumped):
                return False
        return all(self.check(item, schema["items"]) for item in value)

    def string(self, value, schema):
        if not schema.get("minLength", 0) <= len(value) <= schema.get("maxLength", len(value)):
            return False
        return "pattern" not in schema or re.search(schema["pattern"], value) is not None


def schema_for(contract):
    return SchemaSubset(json.loads(read(os.path.join(SCHEMAS,
                                                     contracts.schema_file(contract)))))


def parse(entry):
    path = os.path.normpath(os.path.join(FIXTURES, entry["path"]))
    data = read(path)
    if entry["contract"] in TOML_CONTRACTS:
        return tomlfile.loads(data, path), data
    return json.loads(data.decode("utf-8")), data


def read_with_contract(entry):
    path = os.path.normpath(os.path.join(FIXTURES, entry["path"]))
    data = read(path)
    contract = entry["contract"]
    if contract == "suite":
        return suite.load_bytes(data, path).doc
    if contract == "activation":
        return contracts.validate_activation(tomlfile.loads(data, path), path)
    if contract == "adapters":
        return adapters.load_bytes(data, path)
    if contract == "cli":
        return clicases.load_bytes(data, path)
    if contract == "diagnostics":
        return diagnostics.load_bytes(data, path)
    doc = contracts.read_json(data, contract, path)
    if "companion" in entry:
        cohort = contracts.read_file(os.path.join(FIXTURES, entry["companion"]), "cohort")
        if contract == "calibration-parameters":
            contracts.validate_calibration(doc, path, cohort=cohort)
        else:
            sealed = contracts.read_file(os.path.join(FIXTURES, entry["calibration"]),
                                         "calibration-parameters")
            contracts.validate_readiness(doc, path, cohort=cohort, calibration=sealed)
    return doc


class Parity(unittest.TestCase):
    def setUp(self):
        self.index = json.loads(read(os.path.join(FIXTURES, "index.json")))["fixtures"]

    def test_committed_schemas_are_generated_from_the_readers_definitions(self):
        generated = contracts.schema_documents()
        committed = sorted(name for name in os.listdir(SCHEMAS) if name.startswith("performance-"))
        self.assertEqual(committed, sorted(generated))
        for name, doc in generated.items():
            on_disk = json.loads(read(os.path.join(SCHEMAS, name)))
            self.assertEqual(on_disk, doc, "%s is stale: run python3 scripts/performance/"
                             "schemas.py --write" % name)
            self.assertFalse(doc.get("additionalProperties", False))

    def test_every_contract_ships_valid_and_invalid_golden_examples(self):
        covered = {}
        for entry in self.index:
            covered.setdefault(entry["contract"], set()).add(entry["valid"])
        self.assertEqual(sorted(covered), sorted(contracts.CONTRACTS))
        for contract, kinds in covered.items():
            self.assertEqual(kinds, {True, False}, contract)
        listed = set(os.path.normpath(entry["path"]) for entry in self.index)
        for directory, _, files in os.walk(FIXTURES):
            for name in files:
                relative = os.path.relpath(os.path.join(directory, name), FIXTURES)
                if relative != "index.json":
                    self.assertIn(relative, listed)

    def test_readers_accept_valid_and_refuse_invalid_fixtures(self):
        for entry in self.index:
            with self.subTest(fixture=entry["path"]):
                if entry["valid"]:
                    read_with_contract(entry)
                else:
                    with self.assertRaises(ContractError) as caught:
                        read_with_contract(entry)
                    self.assertIn(entry["error"], str(caught.exception))

    def test_committed_schemas_agree_with_the_fixtures(self):
        for entry in self.index:
            with self.subTest(fixture=entry["path"]):
                try:
                    doc, _ = parse(entry)
                except ContractError:
                    # Refused before any schema applies (a TOML float, for instance).
                    self.assertTrue(not entry["valid"] and entry["shape"])
                    continue
                accepted = schema_for(entry["contract"]).valid(doc)
                if entry["valid"] or not entry["shape"]:
                    self.assertTrue(accepted, "the schema refuses a shape-valid fixture")
                else:
                    self.assertFalse(accepted, "the schema accepts a shape-invalid fixture")

    def test_writers_round_trip_canonical_bytes(self):
        for entry in self.index:
            if not entry["valid"] or entry["contract"] in TOML_CONTRACTS:
                continue
            with self.subTest(fixture=entry["path"]):
                doc = read_with_contract(entry)
                if entry["contract"] == "readiness":
                    continue
                data = contracts.write_bytes(doc, entry["contract"])
                self.assertEqual(contracts.read_json(data, entry["contract"]), doc)
                self.assertTrue(data.endswith(b"}\n") and b"\n" not in data[:-1])

    def test_unknown_versions_fail_closed(self):
        for contract, (schema_id, _, _, _) in contracts.CONTRACTS.items():
            if contract in TOML_CONTRACTS:
                continue
            version = contracts.schema_version(contract)
            newer = "%s/%d" % (schema_id.rsplit("/", 1)[0], version + 1)
            with self.assertRaises(ContractError, msg=contract):
                contracts.read_json(json.dumps({"schema": newer}).encode(), contract)

    def test_earlier_versions_of_changed_shapes_are_refused(self):
        # run/3 freezes the build (its image included) into the recipe identity and names each
        # library instance's seed, builds/5 records the run's execution start and total
        # deadline, shard/4 records that deadline and native seeds, probe-request/2 names a
        # seed copy, golden/2 the seed writers' entries and cli/3 enumerating rules (ADR-0170);
        # verdict/2 carries shard terminal status and coverage; sample/2 records identity,
        # scopes, execution and reasoned host observations (ADR-0168); supervisor-request/3
        # stdout sequence prefixes, supervisor-result/4 their in-order observations and cli/4
        # the stdout_sequence rule (ADR-0171); supervisor-request/4 and supervisor-result/5
        # replace those prefixes by streamed JSON selectors and their observation, and cli/5 the
        # rule by the json_stream rules (ADR-0172); run/4 freezes a diagnostic plan's recipe
        # kind, base and tools (ADR-0179); run/5 a calibration member's injection, cohort/2 its
        # logical members and schedule without Actions run IDs, calibration-parameters/2 the
        # A0 and loop-cost provenance and readiness/2 per-member provenance and whole-trial
        # tallies (ADR-0181); cohort/3 its dispatching producer and run name, and readiness/3
        # each member's original dispatch and attempt beside its nullable run bundle and the
        # refused dispatches (ADR-0182); cohort/4 its dispatching workflow's run-number anchor,
        # and readiness/4 each original's run number and measuring runners, the deletion-free
        # history it was bound from and the parameters recomputed from the originals
        # (ADR-0183); run/6 an active plan's calibrated CPU/image groups, readiness/5 each
        # original's head commit and producer refusal, activation/3 a bridge's evidence record,
        # activation-verification/2 its verified runs, cost/2 observed caches, identities and
        # failed attempts, and shipping/2 the reviewed suite, runs and every planned slot
        # (ADR-0184); builds/6 the observed compiled build cache and its priming, cost/3
        # executions counted per retained run with every execution failure and the observed
        # cache, shipping/3 one target per product identity and environment group with its
        # distribution, and activation-verification/3 a fetched bridge run's producer
        # (ADR-0185); readiness/6 binds each case to its observed shard CPU group
        # (ADR-0194); nothing reads an earlier version.
        changed = ("run", "builds", "verdict", "sample", "shard", "supervisor-request",
                   "supervisor-result", "probe-request", "golden", "cohort",
                   "calibration-parameters", "readiness", "activation-verification", "cost",
                   "shipping")
        self.assertEqual([contracts.schema_version(name) for name in changed],
                         [6, 6, 2, 2, 4, 4, 5, 2, 2, 4, 2, 6, 3, 3, 3])
        self.assertEqual(contracts.schema_version("activation"), 3)
        self.assertEqual((contracts.schema_version("cli"), contracts.schema_version("suite")),
                         (5, 3))
        valid = dict((entry["contract"], entry) for entry in self.index if entry["valid"]
                     and entry["contract"] in changed)
        for contract, entry in sorted(valid.items()):
            doc, _ = parse(entry)
            for version in range(1, contracts.schema_version(contract)):
                doc["schema"] = doc["schema"].rsplit("/", 1)[0] + "/%d" % version
                with self.assertRaises(ContractError) as caught:
                    contracts.read_json(json.dumps(doc).encode(), contract)
                self.assertIn("expected %s" % contracts.CONTRACTS[contract][0],
                              str(caught.exception))
        # Only the laboratory's own files: another family's schema may share a suffix
        # (`telemetry-invocation-cost-v1.json` ends in `cost-v1.json`).
        self.assertFalse(any(name.startswith("performance-")
                             and name.endswith(("run-v1.json", "run-v2.json", "run-v3.json",
                                                "run-v4.json", "run-v5.json", "readiness-v4.json",
                                                "activation-v2.json",
                                                "activation-verification-v1.json", "cost-v1.json",
                                                "shipping-v1.json", "cohort-v1.json",
                                                "activation-verification-v2.json", "cost-v2.json",
                                                "shipping-v2.json", "builds-v5.json",
                                                "cohort-v2.json", "cohort-v3.json",
                                                "activation-v1.json", "readiness-v3.json",
                                                "calibration-parameters-v1.json",
                                                "readiness-v1.json", "readiness-v2.json",
                                                "builds-v1.json",
                                                "builds-v2.json", "builds-v3.json",
                                                "builds-v4.json", "verdict-v1.json",
                                                "sample-v1.json", "shard-v1.json", "shard-v2.json",
                                                "shard-v3.json", "cli-v1.json", "cli-v2.json",
                                                "cli-v3.json", "cli-v4.json",
                                                "supervisor-request-v2.json",
                                                "supervisor-request-v3.json",
                                                "supervisor-result-v3.json",
                                                "supervisor-result-v4.json",
                                                "suite-v1.json", "suite-v2.json",
                                                "golden-v1.json", "probe-request-v1.json",
                                                "supervisor-request-v1.json",
                                                "supervisor-result-v1.json",
                                                "supervisor-result-v2.json"))
                                 for name in os.listdir(SCHEMAS)))

    def test_probe_control_messages_are_bounded(self):
        entry = [e for e in self.index if e["contract"] == "probe" and e["valid"]][0]
        doc, _ = parse(entry)
        line = contracts.write_bytes(doc, "probe")
        self.assertEqual(contracts.read_probe_line(line), doc)
        doc["correctness"]["detail"] = "x" * 4096
        doc["parameters"] = dict(("p%d" % i, "v" * 60) for i in range(4))
        big = json.dumps(dict(doc, metrics=dict(("m%d" % i, {"value": "1" * 38, "unit": "ms"})
                                               for i in range(32)))).encode()
        big += b" " * (contracts.limits.RECORD_BYTES_MAX - len(big) + 1)
        with self.assertRaises(ContractError):
            contracts.read_probe_line(big)


if __name__ == "__main__":
    unittest.main()
