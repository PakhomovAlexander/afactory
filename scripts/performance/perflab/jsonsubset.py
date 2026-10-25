"""Validate a document against a committed JSON Schema of the product (ADR-0213).

The laboratory reads the documents `af telemetry ... --json` and the telemetry bench scripts
print through the schemas the product commits beside its own parity tests
(`schemas/telemetry-*.json`), never through a second, more permissive reader of its own. This
evaluator implements exactly the JSON Schema 2020-12 keywords those schemas use and refuses a
schema that uses any other, so a keyword it does not understand can never pass a document
silently. It judges shape only; the laboratory's semantic rules apply afterwards.
"""
import json
import os
import re

from .values import ContractError, require

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
SCHEMAS = os.path.join(ROOT, "schemas")
# Annotation keywords carry no assertion.
ANNOTATIONS = {"$schema", "$id", "title", "description", "$comment", "examples", "default"}
ASSERTIONS = {"$defs", "$ref", "type", "properties", "required", "additionalProperties",
              "patternProperties", "minProperties", "maxProperties", "items", "minItems",
              "maxItems", "uniqueItems", "prefixItems", "enum", "const", "pattern", "minLength",
              "maxLength", "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "anyOf",
              "oneOf"}
TYPES = ("object", "array", "string", "integer", "number", "boolean", "null")
DEPTH_MAX = 64


def _typed(value, kind):
    if kind == "object":
        return isinstance(value, dict)
    if kind == "array":
        return isinstance(value, list)
    if kind == "string":
        return isinstance(value, str)
    if kind == "integer":
        return type(value) is int
    if kind == "number":
        return type(value) in (int, float)
    if kind == "boolean":
        return type(value) is bool
    return value is None


def _same(left, right):
    """JSON equality: a boolean is never a number, and 1 equals 1.0."""
    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        return left == right
    if isinstance(left, list) and isinstance(right, list):
        return len(left) == len(right) and all(_same(a, b) for a, b in zip(left, right))
    if isinstance(left, dict) and isinstance(right, dict):
        return set(left) == set(right) and all(_same(left[key], right[key]) for key in left)
    return type(left) is type(right) and left == right


class Schema(object):
    """One committed schema, its keywords checked once when loaded."""

    def __init__(self, doc, name):
        require(isinstance(doc, dict), name + ": a schema is an object")
        self.doc = doc
        self.name = name
        self.patterns = {}
        self._check(doc, name, 0)

    def _check(self, node, where, depth):
        require(depth <= DEPTH_MAX, where + ": nested too deeply")
        if isinstance(node, bool):
            return
        require(isinstance(node, dict), where + ": a schema is an object or a boolean")
        unknown = sorted(set(node) - ASSERTIONS - ANNOTATIONS)
        require(not unknown, "%s: uses keywords this evaluator does not implement: %s"
                % (where, ", ".join(unknown)))
        if "type" in node:
            kinds = node["type"] if isinstance(node["type"], list) else [node["type"]]
            require(kinds and all(kind in TYPES for kind in kinds), where + ".type: unknown")
        for key in ("pattern",):
            if key in node:
                self.patterns[node[key]] = re.compile(node[key])
        if "patternProperties" in node:
            for pattern in node["patternProperties"]:
                self.patterns[pattern] = re.compile(pattern)
        for key in ("properties", "$defs", "patternProperties"):
            for name, child in node.get(key, {}).items():
                self._check(child, "%s.%s.%s" % (where, key, name), depth + 1)
        for key in ("items", "additionalProperties"):
            if key in node and not isinstance(node[key], bool):
                self._check(node[key], "%s.%s" % (where, key), depth + 1)
        for key in ("anyOf", "oneOf", "prefixItems"):
            for index, child in enumerate(node.get(key, ())):
                self._check(child, "%s.%s[%d]" % (where, key, index), depth + 1)
        if "$ref" in node:
            reference = node["$ref"]
            require(isinstance(reference, str) and reference.startswith("#/$defs/")
                    and reference[len("#/$defs/"):] in self.doc.get("$defs", {}),
                    where + ".$ref: names no definition of this schema")

    def errors(self, value, where="document"):
        """The first violation as text, or None."""
        return self._errors(value, self.doc, where, 0)

    def validate(self, value, where="document"):
        error = self.errors(value, where)
        require(error is None, "%s: refused by %s: %s" % (where, self.name, error))
        return value

    def _errors(self, value, node, where, depth):
        if depth > DEPTH_MAX:
            return where + ": nested too deeply"
        if node is True:
            return None
        if node is False:
            return where + ": no value is allowed here"
        if "$ref" in node:
            target = self.doc["$defs"][node["$ref"][len("#/$defs/"):]]
            error = self._errors(value, target, where, depth + 1)
            if error is not None:
                return error
        if "anyOf" in node and not any(self._errors(value, child, where, depth + 1) is None
                                       for child in node["anyOf"]):
            return where + ": matches no alternative"
        if "oneOf" in node and sum(1 for child in node["oneOf"]
                                   if self._errors(value, child, where, depth + 1) is None) != 1:
            return where + ": does not match exactly one alternative"
        if "const" in node and not _same(value, node["const"]):
            return where + ": is not %s" % json.dumps(node["const"])[:64]
        if "enum" in node and not any(_same(value, item) for item in node["enum"]):
            return where + ": is not one of the declared values"
        if "type" in node:
            kinds = node["type"] if isinstance(node["type"], list) else [node["type"]]
            if not any(_typed(value, kind) for kind in kinds):
                return where + ": expected " + " or ".join(kinds)
        if isinstance(value, dict):
            return self._object(value, node, where, depth)
        if isinstance(value, list):
            return self._array(value, node, where, depth)
        if isinstance(value, str):
            if not node.get("minLength", 0) <= len(value) <= node.get("maxLength", len(value)):
                return where + ": length out of bounds"
            if "pattern" in node and self.patterns[node["pattern"]].search(value) is None:
                return where + ": does not match its pattern"
        if type(value) in (int, float):
            if "minimum" in node and value < node["minimum"]:
                return where + ": below its minimum"
            if "maximum" in node and value > node["maximum"]:
                return where + ": above its maximum"
            if "exclusiveMinimum" in node and value <= node["exclusiveMinimum"]:
                return where + ": not above its exclusive minimum"
            if "exclusiveMaximum" in node and value >= node["exclusiveMaximum"]:
                return where + ": not below its exclusive maximum"
        return None

    def _object(self, value, node, where, depth):
        properties = node.get("properties", {})
        patterns = node.get("patternProperties", {})
        for key in node.get("required", ()):
            if key not in value:
                return "%s: missing %s" % (where, key)
        if not node.get("minProperties", 0) <= len(value) <= node.get("maxProperties",
                                                                        len(value)):
            return where + ": member count out of bounds"
        extra = node.get("additionalProperties", True)
        for key, item in value.items():
            at = "%s.%s" % (where, key[:64])
            matched = False
            if key in properties:
                matched = True
                error = self._errors(item, properties[key], at, depth + 1)
                if error is not None:
                    return error
            for pattern, child in patterns.items():
                if self.patterns[pattern].search(key) is not None:
                    matched = True
                    error = self._errors(item, child, at, depth + 1)
                    if error is not None:
                        return error
            if not matched:
                if extra is False:
                    return "%s: unknown member %s" % (where, json.dumps(key[:64]))
                if isinstance(extra, dict):
                    error = self._errors(item, extra, at, depth + 1)
                    if error is not None:
                        return error
        return None

    def _array(self, value, node, where, depth):
        if not node.get("minItems", 0) <= len(value) <= node.get("maxItems", len(value)):
            return where + ": item count out of bounds"
        if node.get("uniqueItems"):
            seen = set()
            for item in value:
                key = json.dumps(item, sort_keys=True)
                if key in seen:
                    return where + ": items are not unique"
                seen.add(key)
        prefix = node.get("prefixItems", ())
        for index, (item, child) in enumerate(zip(value, prefix)):
            error = self._errors(item, child, "%s[%d]" % (where, index), depth + 1)
            if error is not None:
                return error
        if "items" in node:
            for index, item in enumerate(value[len(prefix):], len(prefix)):
                error = self._errors(item, node["items"], "%s[%d]" % (where, index), depth + 1)
                if error is not None:
                    return error
        return None


_LOADED = {}


def load(name, root=None):
    """The committed schema `schemas/NAME` of the checkout at `root` (default: this one)."""
    path = os.path.join(root or ROOT, "schemas", name)
    key = os.path.abspath(path)
    if key not in _LOADED:
        require(re.match(r"^[a-z0-9-]{1,96}\.json$", name) is not None,
                "%s: not a schema file name" % name)
        try:
            with open(path, "rb") as stream:
                data = stream.read(1024 * 1024 + 1)
        except OSError as error:
            raise ContractError("%s: cannot read the committed schema: %s" % (name, error))
        require(len(data) <= 1024 * 1024, name + ": a schema over 1 MiB")
        _LOADED[key] = Schema(json.loads(data.decode("utf-8")), name)
    return _LOADED[key]


def _unique(pairs):
    out = {}
    for key, value in pairs:
        require(key not in out, "a duplicate member %s" % json.dumps(key[:64]))
        out[key] = value
    return out


def _refuse_constant(text):
    raise ContractError("a non-finite number %s" % text)


def parse(data, where, limit):
    """One JSON document of at most `limit` bytes, UTF-8, without duplicate members or
    non-finite numbers. Floats are allowed: these are the product's documents, not the
    laboratory's canonical evidence."""
    require(isinstance(data, bytes) and len(data) <= limit,
            "%s: over its %d-byte bound" % (where, limit))
    try:
        return json.loads(data.decode("utf-8"), object_pairs_hook=_unique,
                          parse_constant=_refuse_constant)
    except (UnicodeDecodeError, ValueError) as error:
        raise ContractError("%s: not one JSON document: %s" % (where, str(error)[:512]))
