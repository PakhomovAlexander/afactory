"""A small closed-shape language: one definition validates a document and emits its JSON Schema.

The readers validate shape through these nodes and then apply their semantic rules; the
committed `schemas/performance-*.json` files are generated from the same nodes, and a parity
test refuses any difference, so a schema and its reader cannot drift.
"""
import re

from .values import (ContractError, DECIMAL_PATTERN, DIGEST_PATTERN, GIT_SHA_PATTERN,
                     ID_PATTERN, RATIONAL_PATTERN, check_decimal,
                     check_rational, check_relative_path, require)


class Node:
    def validate(self, value, where):
        raise NotImplementedError

    def schema(self, defs):
        raise NotImplementedError


class Str(Node):
    def __init__(self, pattern=None, enum=None, min_len=0, max_len=4096, check=None):
        self.pattern = pattern
        self.compiled = re.compile(pattern) if pattern else None
        self.enum = tuple(enum) if enum else None
        self.min_len = min_len
        self.max_len = max_len
        self.check = check

    def validate(self, value, where):
        require(isinstance(value, str), where + ": expected a string")
        require(self.min_len <= len(value) <= self.max_len,
                where + ": string length must be within %d..%d" % (self.min_len, self.max_len))
        if self.enum is not None:
            require(value in self.enum, where + ": expected one of " + ", ".join(self.enum))
        if self.compiled is not None:
            require(self.compiled.match(value) is not None,
                    where + ": does not match " + self.pattern)
        if self.check is not None:
            self.check(value, where)
        return value

    def schema(self, defs):
        out = {"type": "string"}
        if self.enum is not None:
            out["enum"] = list(self.enum)
        else:
            if self.min_len:
                out["minLength"] = self.min_len
            out["maxLength"] = self.max_len
            if self.pattern:
                out["pattern"] = self.pattern
        return out


class Int(Node):
    def __init__(self, minimum=0, maximum=2 ** 63 - 1):
        self.minimum = minimum
        self.maximum = maximum

    def validate(self, value, where):
        require(type(value) is int, where + ": expected an integer")
        require(self.minimum <= value <= self.maximum,
                where + ": must be within %d..%d" % (self.minimum, self.maximum))
        return value

    def schema(self, defs):
        return {"type": "integer", "minimum": self.minimum, "maximum": self.maximum}


class Bool(Node):
    def validate(self, value, where):
        require(type(value) is bool, where + ": expected a boolean")
        return value

    def schema(self, defs):
        return {"type": "boolean"}


class Const(Node):
    def __init__(self, value):
        self.value = value

    def validate(self, value, where):
        require(type(value) is type(self.value) and value == self.value,
                where + ": expected %r" % (self.value,))
        return value

    def schema(self, defs):
        return {"const": self.value}


class Nullable(Node):
    def __init__(self, inner):
        self.inner = inner

    def validate(self, value, where):
        if value is None:
            return None
        return self.inner.validate(value, where)

    def schema(self, defs):
        return {"anyOf": [{"type": "null"}, self.inner.schema(defs)]}


class AnyOf(Node):
    """The first alternative that accepts the value; the alternatives must not overlap."""

    def __init__(self, *alternatives):
        self.alternatives = alternatives

    def validate(self, value, where):
        errors = []
        for node in self.alternatives:
            try:
                return node.validate(value, where)
            except ContractError as error:
                errors.append(str(error))
        raise ContractError("; or ".join(errors))

    def schema(self, defs):
        return {"anyOf": [node.schema(defs) for node in self.alternatives]}


class Arr(Node):
    def __init__(self, items, min_items=0, max_items=4096, unique=False):
        self.items = items
        self.min_items = min_items
        self.max_items = max_items
        self.unique = unique

    def validate(self, value, where):
        require(isinstance(value, list), where + ": expected an array")
        require(self.min_items <= len(value) <= self.max_items,
                where + ": must hold %d..%d items" % (self.min_items, self.max_items))
        out = [self.items.validate(item, "%s[%d]" % (where, index))
               for index, item in enumerate(value)]
        if self.unique:
            seen = []
            for item in value:
                require(item not in seen, where + ": items must be unique")
                seen.append(item)
        return out

    def schema(self, defs):
        out = {"type": "array", "items": self.items.schema(defs), "minItems": self.min_items,
               "maxItems": self.max_items}
        if self.unique:
            out["uniqueItems"] = True
        return out


class JsonObject(Node):
    """Any JSON object; its reader's semantic rules check what it holds."""

    def validate(self, value, where):
        require(isinstance(value, dict), where + ": expected an object")
        return value

    def schema(self, defs):
        return {"type": "object"}


class Field:
    def __init__(self, name, node, required=True):
        self.name = name
        self.node = node
        self.required = required


class Obj(Node):
    """A closed object: unknown fields fail, required fields must be present."""

    def __init__(self, *fields):
        self.fields = fields
        self.by_name = dict((field.name, field) for field in fields)

    def validate(self, value, where):
        require(isinstance(value, dict), where + ": expected an object")
        unknown = sorted(set(value) - set(self.by_name))
        require(not unknown, where + ": unknown field " + ", ".join(unknown))
        for field in self.fields:
            if field.name in value:
                field.node.validate(value[field.name], where + "." + field.name)
            else:
                require(not field.required, where + ": missing field " + field.name)
        return value

    def schema(self, defs):
        props = dict((field.name, field.node.schema(defs)) for field in self.fields)
        out = {"type": "object", "additionalProperties": False, "properties": props}
        required = [field.name for field in self.fields if field.required]
        if required:
            out["required"] = required
        return out


class Map(Node):
    """An object whose keys follow a pattern and whose values share one shape."""

    def __init__(self, key_pattern, value, min_props=0, max_props=256):
        self.key_pattern = key_pattern
        self.compiled = re.compile(key_pattern)
        self.value = value
        self.min_props = min_props
        self.max_props = max_props

    def validate(self, value, where):
        require(isinstance(value, dict), where + ": expected an object")
        require(self.min_props <= len(value) <= self.max_props,
                where + ": must hold %d..%d entries" % (self.min_props, self.max_props))
        for key, item in value.items():
            require(self.compiled.match(key) is not None,
                    where + ": key %r does not match %s" % (key, self.key_pattern))
            self.value.validate(item, where + "." + key)
        return value

    def schema(self, defs):
        return {"type": "object", "minProperties": self.min_props,
                "maxProperties": self.max_props,
                "patternProperties": {self.key_pattern: self.value.schema(defs)},
                "additionalProperties": False}


class Tagged(Node):
    """A closed union of objects discriminated by one constant field."""

    def __init__(self, tag, variants):
        self.tag = tag
        self.variants = variants

    def validate(self, value, where):
        require(isinstance(value, dict), where + ": expected an object")
        kind = value.get(self.tag)
        require(isinstance(kind, str) and kind in self.variants,
                where + "." + self.tag + ": expected one of " + ", ".join(sorted(self.variants)))
        return self.variants[kind].validate(value, where)

    def schema(self, defs):
        return {"oneOf": [self.variants[key].schema(defs) for key in sorted(self.variants)]}


class Named(Node):
    """A shared definition, emitted once under `$defs`."""

    def __init__(self, name, inner, description=None):
        self.name = name
        self.inner = inner
        self.description = description

    def validate(self, value, where):
        return self.inner.validate(value, where)

    def schema(self, defs):
        if self.name not in defs:
            defs[self.name] = None
            body = self.inner.schema(defs)
            if self.description:
                body = dict(body, description=self.description)
            defs[self.name] = body
        return {"$ref": "#/$defs/" + self.name}


def tagged_variant(tag, value, *fields):
    return Obj(Field(tag, Const(value)), *fields)


def json_schema(urn, title, description, node):
    defs = {}
    body = node.schema(defs)
    out = {"$schema": "https://json-schema.org/draft/2020-12/schema", "$id": urn,
           "title": title, "description": description}
    out.update(body)
    if defs:
        out["$defs"] = dict(sorted(defs.items()))
    return out


# Shared primitives -------------------------------------------------------------------------

ID = Named("id", Str(pattern=ID_PATTERN, max_len=64))
GIT_SHA = Named("git_sha", Str(pattern=GIT_SHA_PATTERN, max_len=64))
DIGEST = Named("digest", Str(pattern=DIGEST_PATTERN, max_len=71))
DECIMAL = Named("decimal", Str(pattern=DECIMAL_PATTERN, max_len=80, check=check_decimal),
                "Canonical nonnegative decimal, at most 38 significant digits.")
RATIONAL = Named("rational", Str(pattern=RATIONAL_PATTERN, max_len=512, check=check_rational),
                 "Exact signed rational in lowest terms, such as -3/4 or 2.")
UNIT = Named("unit", Str(enum=("ms", "bytes", "count", "ratio")))
REL_PATH = Named("relative_path", Str(pattern=r"^[A-Za-z0-9._-]+(?:/[A-Za-z0-9._-]+)*$",
                                      max_len=1024, check=check_relative_path))
TEXT = Str(max_len=4096)
SHORT_TEXT = Str(min_len=1, max_len=256)
COUNT = Int(0, 2 ** 63 - 1)
METRIC = Named("metric", Obj(Field("value", DECIMAL), Field("unit", UNIT)))
