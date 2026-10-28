"""Telemetry experiments: Telemetry's own cost, measured beside the comparison (ADR-0214,
ADR-0215).

`cli.py telemetry-run` measures a baseline and a candidate AF binary with the credential-free
bench scripts of design section 12, exactly as `fixtures/performance/telemetry.toml` declares
them: `scripts/telemetry-overhead.py` (`off` against `spans` on three bench fixtures),
`scripts/telemetry-invocation-cost.py` (Claude `json` against `stream-json` at `off` and at
`spans`) and `scripts/telemetry-corpus.py` (read latency, peak RSS and index size over one
deterministic corpus this checkout generates once). Both sides run on this host with this
checkout's scripts, fixtures and settings: one measurement authority, identified by the content
of every file it reads (`authority_digest`). Rounds alternate which side runs first, and both
sides of a round use the same seed.

Every execution, each side's `--version` query and the corpus generator included, runs through
one `telemetryexec` executor within one monotonic deadline: in its own Linux container of the
pinned image (`linux_container`, the only mode a pull request is measured in) or, for source the
operator trusts, on this host (`trusted_local`, recorded as such). Each has a private HOME and
TMPDIR in its own new directory and no `AF_*`, `XDG_*` or `OTEL_*` variable; the scripts give
every af command private homes and a Store of their own. What it writes, its corpus copy
included, is held to the profile's work bytes while it writes. After each execution both
binaries, the measurement authority, the master corpus and the result directory are checked
again; anything changed stops the run. An uncertain cleanup stops it too, and its work is kept
for the operator. A run's stdout is its one JSON document, retained byte for byte and read
through the product's committed schema; its stderr's tail is retained. Every planned run is
recorded once, with why it failed, timed out, was refused or never ran.

A document is admitted only when it is the run it was planned as: the configured pairs, scale,
seed, repeats, fixtures and cases, this side's own binary version, and for read latency the
corpus this result generated, by its runs, seed and files. Each script's own rule then judges it
(full scale and the minimum pairs are the script's), recomputed from its samples: every
difference, median, interval, latency, bound and verdict the document records must be what its
script's own `summarize` (or, for read latency, its sample protocol and declared bounds) gives
its raw pairs and samples. Beside it, `within` keeps each side's own figures by level, and
`comparisons` sets the two sides side by side per round, level, metric and shape, only from
documents whose protocol, corpus and machine agree and only on one host; anything else is
`incompatible` or `missing` with its reason, never a figure. The telemetry level is an explicit
dimension throughout: `off` and `spans` are never folded together. Nothing here is a verdict
sample, a required check or an input to the comparison's verdict.

`verify` reads a result bundle back offline under exactly the measurement authority it was
measured with: a result of another authority is refused, never kept as recorded. `history`
keeps only results verified so, in a durable root held under one lock, within its byte, result
and retention bounds, and groups them by configuration, authority, execution mode and host.
"""
import copy
import hashlib
import importlib.util
import json
import math
import os
import re
import stat
import shutil
import sys
import time
from fractions import Fraction
from dataclasses import dataclass
from types import MappingProxyType

from . import budget as deadlines
from . import admission, bundle, contracts, host, jsonsubset, render, telemetryexec, tomlfile
from .values import (ContractError, add_note, canonical_digest, check_relative_path,
                     format_decimal, format_rational, require, sha256_bytes)

ROOT = jsonsubset.ROOT
REGISTRY_PATH = "fixtures/performance/telemetry.toml"
# The laboratory's own code that runs the scripts, reads the documents and derives every figure,
# beside the registry's declared inputs and the product schemas.
OWN_INPUTS = ("scripts/performance/perflab", "scripts/performance/cli.py", "schemas",
              "fixtures/performance", ".github/workflows/performance-telemetry.yml",
              "Cargo.toml", "Cargo.lock", "rust-toolchain.toml")
RESULT_JSON = "result.json"
REPORT_HTML = "report.html"
SUMMARY_MD = "summary.md"
RAW = "raw"
LOGS = "logs"
DOCUMENT_BYTES_MAX = telemetryexec.DOCUMENT_BYTES_MAX
LOG_BYTES_MAX = telemetryexec.LOG_BYTES_MAX
BUNDLE_BYTES_MAX = 512 * 1024 * 1024
AUTHORITY_FILES_MAX = 4096
AUTHORITY_BYTES_MAX = 512 * 1024 * 1024
PATH = "/usr/bin:/bin:/usr/local/bin"
# Kept before the run's deadline for the last execution's teardown, the integrity checks and
# writing the result.
RESERVE_SECONDS = 90
# The largest file any execution may write (RLIMIT_FSIZE), within its profile's work bytes.
FILE_BYTES_MAX = 4 * 1024 * 1024 * 1024
READ_MEASUREMENTS = ("fresh_status", "unchanged_status", "show")
CORPUS_FACTS = ("segments", "segment_bytes", "segment_stored_bytes", "blob_files",
                "blob_stored_bytes")
SIDES = contracts.SIDES
SHAPES = ("json", "stream-json")
EXIT_GATE_FAILED = 3
CORPUS_NAME_FULL = "telemetry-corpus-10k"


class Unavailable(ContractError):
    def __init__(self, reason, detail):
        ContractError.__init__(self, detail)
        self.reason = reason


# The registry and the measurement authority ------------------------------------------------

def load_registry(root=None, budget=None):
    """The validated registry of the checkout at `root`, and its SHA-256."""
    path = os.path.join(root or ROOT, REGISTRY_PATH)
    fd, info = admission.open_confined(root or ROOT, REGISTRY_PATH, REGISTRY_PATH)
    try:
        require(info.st_size <= tomlfile.MAX_TOML_BYTES, "telemetry registry exceeds its byte bound")
        chunks = []
        admission.read_bounded(fd, info.st_size, budget, path, "reading telemetry registry", chunks.append)
        _unchanged(fd, info, path)
        data = b"".join(chunks)
    finally:
        os.close(fd)
    doc = contracts.validate_telemetry_lab(tomlfile.loads(data, REGISTRY_PATH), REGISTRY_PATH)
    return doc, sha256_bytes(data)


def authority_files(registry, root=None, budget=None, identities=None):
    """Every file the measurement authority reads, relative to `root`, sorted: the registry,
    each experiment's declared inputs (a directory whole), the product schemas its documents
    are read through, and this module. A link, a special file or an escape is refused."""
    root = root or ROOT
    files = []
    bounds = admission.Bounds(AUTHORITY_FILES_MAX, 64, AUTHORITY_BYTES_MAX,
                              AUTHORITY_BYTES_MAX)
    for name in authority_names(registry):
        admission.check(budget, "enumerating telemetry authority")
        # Reach every declared directory through held descriptors too: scan(path) alone
        # would permit a symlink in an ancestor of a declared input.
        parent = admission.open_root(root, "telemetry authority")
        try:
            parts = name.split("/")
            for part in parts[:-1]:
                child = os.open(part, admission.DIRECTORY, dir_fd=parent)
                os.close(parent)
                parent = child
            info = os.stat(parts[-1], dir_fd=parent, follow_symlinks=False)
            if stat.S_ISDIR(info.st_mode):
                directory = os.open(parts[-1], admission.DIRECTORY, dir_fd=parent)
                try:
                    admission._opened(directory, info, stat.S_IFDIR, name)
                    entries = []
                    admission._scan(directory, "", 1, entries, {"bytes": 0}, bounds,
                                    budget, name, "enumerating telemetry authority")
                    for entry in entries:
                        if entry.kind == "file":
                            relative = name + "/" + entry.path
                            files.append(relative)
                            if identities is not None:
                                identities[relative] = entry.identity + (entry.size,)
                finally:
                    os.close(directory)
            else:
                require(stat.S_ISREG(info.st_mode), "%s: not a regular file" % name)
                files.append(name)
                if identities is not None:
                    identities[name] = (info.st_dev, info.st_ino, info.st_size)
        finally:
            os.close(parent)
        require(len(files) <= AUTHORITY_FILES_MAX,
                "the measurement authority holds too many files")
    require(all("/__pycache__/" not in "/" + name and not name.endswith((".pyc", ".pyo"))
                for name in files), "bytecode cache is not measurement authority")
    return sorted(set(files))


def authority_digest(registry, root=None, budget=None):
    """(digest, file count), using confined, bounded, identity-checked reads."""
    root = root or ROOT
    entries, total, identities = [], 0, {}
    for relative in authority_files(registry, root, budget, identities):
        fd, info = admission.open_confined(root, relative, "measurement authority")
        try:
            require((info.st_dev, info.st_ino, info.st_size) == identities[relative],
                    "%s changed after authority enumeration" % relative)
            total += info.st_size
            require(total <= AUTHORITY_BYTES_MAX, "the measurement authority exceeds its byte bound")
            digest = admission.read_bounded(fd, info.st_size, budget, relative, "authority hashing")
            _unchanged(fd, info, os.path.join(root, relative))
            entries.append([relative, digest])
        finally:
            os.close(fd)
    return canonical_digest({"schema": "af.perf-telemetry-authority/2", "files": entries}), len(entries)


def configuration(registry, profile, experiments=None, rounds=None, pairs=None, scale=None,
                  corpus_runs=None):
    """The effective configuration of one run of `profile`, explicit overrides applied within
    the caps, and its digest: what two results must share to be compared."""
    require(profile in registry["profiles"], "--profile: one of " + ", ".join(
        contracts.TELEMETRY_PROFILES))
    chosen = registry["profiles"][profile]
    selected = list(experiments) if experiments else list(chosen["experiments"])
    for name in selected:
        require(name in contracts.TELEMETRY_EXPERIMENTS, "--experiment: one of " + ", ".join(
            contracts.TELEMETRY_EXPERIMENTS))
    require(len(selected) == len(set(selected)), "--experiment: each once")
    doc = {"seed": registry["seed"],
           "rounds": chosen["rounds"] if rounds is None else rounds,
           "experiments": [name for name in contracts.TELEMETRY_EXPERIMENTS if name in selected],
           "overhead": dict(chosen["overhead"]),
           "invocation_cost": dict(chosen["invocation_cost"]),
           "read_latency": dict(chosen["read_latency"]),
           "timeout_minutes": chosen["timeout_minutes"], "work_bytes": chosen["work_bytes"]}
    for name in ("overhead", "invocation_cost"):
        if pairs is not None:
            doc[name]["pairs"] = pairs
        if scale is not None:
            require(scale in ("full", "smoke"), "--scale: full or smoke")
            doc[name]["scale"] = scale
    if corpus_runs is not None:
        doc["read_latency"]["runs"] = corpus_runs
    require(1 <= doc["rounds"] <= contracts.TELEMETRY_ROUNDS_MAX,
            "--rounds: 1 to %d" % contracts.TELEMETRY_ROUNDS_MAX)
    require(pairs is None or 1 <= pairs <= contracts.TELEMETRY_PAIRS_MAX,
            "--pairs: 1 to %d" % contracts.TELEMETRY_PAIRS_MAX)
    doc["digest"] = contracts.telemetry_configuration_digest(doc)
    contracts.TELEMETRY_CONFIGURATION.validate(doc, "configuration")
    return doc


def argv_for(experiment, settings, seed, script, af="{af}", work="{work}", corpus="{corpus}",
             python="python3"):
    """The script's argv; with the defaults, the recorded form with its placeholders."""
    base = [python, "-I", "-B", script]
    if experiment == "read_latency":
        return base + ["measure", "--dir", corpus, "--af", af, "--repeat",
                       str(settings["repeat"]), "--json"]
    return base + ["--af", af, "--pairs", str(settings["pairs"]), "--scale", settings["scale"],
                   "--seed", str(seed), "--work", work, "--json"]


def corpus_argv(settings, seed, script, out="{corpus}", python="python3"):
    return [python, "-I", "-B", script, "generate", "--out", out, "--runs",
            str(settings["runs"]), "--seed", str(seed), "--jobs", "1"]


def private_environment(home):
    """A script's environment: a fixed PATH, the C locale, UTC and a private HOME and TMPDIR,
    nothing of the caller's; the scripts give every af command homes of its own."""
    return {"PATH": PATH, "LC_ALL": "C", "LANG": "C", "TZ": "UTC", "HOME": home,
            "TMPDIR": home, "NO_COLOR": "1", "PYTHONDONTWRITEBYTECODE": "1"}


# The generated corpus ---------------------------------------------------------------------

def _trailer_size(path):
    """The uncompressed size a gzip member records in its trailer."""
    with open(path, "rb") as stream:
        stream.seek(-4, os.SEEK_END)
        return int.from_bytes(stream.read(4), "little")


def corpus_facts(corpus):
    """The file-level facts of a generated corpus, read by this controller from its files: what
    every read-latency document must report of the corpus it read."""
    segments = os.path.join(corpus, "v1", "segments")
    blobs = os.path.join(corpus, "v1", "blobs")
    facts = dict((name, 0) for name in CORPUS_FACTS)
    require(os.path.isdir(segments) and not os.path.islink(segments),
            "the corpus holds no v1/segments directory")
    for name in sorted(os.listdir(segments)):
        path = os.path.join(segments, name)
        require(not os.path.islink(path), "the corpus holds a link: %s" % name)
        if name.endswith(".jsonl.gz") and os.path.isfile(path):
            facts["segments"] += 1
            facts["segment_bytes"] += _trailer_size(path)
            facts["segment_stored_bytes"] += os.lstat(path).st_size
    if os.path.isdir(blobs):
        for directory in sorted(os.listdir(blobs)):
            inner = os.path.join(blobs, directory)
            require(not os.path.islink(inner), "the corpus holds a link: %s" % directory)
            if not os.path.isdir(inner):
                continue
            for name in sorted(os.listdir(inner)):
                path = os.path.join(inner, name)
                require(not os.path.islink(path), "the corpus holds a link: %s" % name)
                if name.endswith(".gz") and os.path.isfile(path):
                    facts["blob_files"] += 1
                    facts["blob_stored_bytes"] += os.lstat(path).st_size
    return facts


def inventory(root):
    """{relative path: (size, mtime ns, inode, mode)} of every entry below `root`, nothing
    followed: what must not change while another execution runs."""
    entries = admission.scan(root, admission.Bounds(1000000, 64, FILE_BYTES_MAX,
                                                    FILE_BYTES_MAX * 16))
    return dict((entry.path, (entry.size, info.st_mtime_ns, info.st_ino, info.st_mode))
                for entry in entries for info in [os.lstat(os.path.join(root, entry.path))])


# Reading a document and its script's own judgment -----------------------------------------

_MODULES = {}


def script_module(script, root=None):
    """The bench script as a module, loaded from the checkout at `root` without writing
    bytecode; only ever a trusted measurement authority's own file, never a result's."""
    path = os.path.join(root or ROOT, script)
    with open(path, "rb") as stream:
        data = stream.read(AUTHORITY_BYTES_MAX + 1)
    key = (os.path.abspath(path), sha256_bytes(data))
    if key not in _MODULES:
        previous = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            name = "af_bench_" + hashlib.sha256(repr(key).encode("utf-8")).hexdigest()[:12]
            spec = importlib.util.spec_from_file_location(name, path)
            module = importlib.util.module_from_spec(spec)
            exec(compile(data, path, "exec"), module.__dict__)
        finally:
            sys.dont_write_bytecode = previous
        _MODULES[key] = module
    return _MODULES[key]


@dataclass(frozen=True)
class _NumericToken:
    text: str


class MeasurementRefusal(ContractError):
    """A known numeric field refused before any summarization or comparison."""

    def __init__(self, field, token, reason):
        self.field, self.token, self.reason = field, token, reason
        ContractError.__init__(self, "%s: %s (numeric token %s)" % (
            field, reason, token[:512]))


def _numeric_fields(doc, schema, definitions, path=""):
    """Walk only schema-declared fields. Candidate keys and free text never enter evidence."""
    if "$ref" in schema:
        yield from _numeric_fields(doc, definitions[schema["$ref"].split("/")[-1]],
                                   definitions, path)
        return
    for child in schema.get("oneOf", ()):
        yield from _numeric_fields(doc, child, definitions, path)
    if isinstance(doc, dict):
        for key, child in schema.get("properties", {}).items():
            if key in doc:
                yield from _numeric_fields(doc[key], child, definitions,
                                           path + ("." if path else "") + key)
    elif isinstance(doc, list) and "items" in schema:
        for index, value in enumerate(doc):
            # Evidence must resolve through the same declared bounds offline. The
            # complete document still goes through ordinary schema admission below;
            # excess entries are never admitted by truncating selected samples.
            if index >= schema.get("maxItems", 100000000):
                break
            yield from _numeric_fields(value, schema["items"], definitions,
                                       "%s[%d]" % (path, index))
    else:
        kinds = schema.get("type", ())
        kinds = [kinds] if isinstance(kinds, str) else kinds
        if "number" in kinds or "integer" in kinds:
            if isinstance(doc, _NumericToken):
                yield path, doc.text, schema
            elif type(doc) in (int, float):
                yield path, json.dumps(doc), schema


def _numeric_reason(path, token, schema):
    if len(token) > 512:
        return "token_too_long"
    # Inspect the bounded original mantissa before float parsing loses its sign/value
    # through underflow. Never expand an exponent or construct an exact huge power.
    mantissa = re.split("[eE]", token, maxsplit=1)[0]
    nonzero = any(digit in "123456789" for digit in mantissa)
    value = json.loads(token)
    if isinstance(value, float) and not math.isfinite(value):
        return "nonfinite"
    kinds = schema["type"]
    if "integer" in kinds and type(value) is not int:
        return "not_integer"
    # Differences, percentages and their intervals are signed. These physical quantities
    # and every individual latency sample are not, even when a median hides a bad sample.
    physical = re.search(r"(?:^|\.)(?:wall_ms|drain_ms|off_ms|spans_ms|latency_ms|"
                         r"peak_rss_bytes|index_bytes|stdout_bytes|segments|load1|"
                         r"samples_ms\[[0-9]+\])$", path)
    if token.startswith("-") and nonzero and (physical or schema.get("minimum", -1) >= 0):
        return "negative"
    if nonzero and value == 0:
        return "unrepresentable"
    if (path.endswith(".wall_ms") or schema.get("exclusiveMinimum") == 0) and value <= 0:
        return "not_positive"
    try:
        number = _number(value, path)
        if physical or schema.get("minimum", -1) >= 0:
            contracts.DECIMAL.validate(format_decimal(number, path), path)
        else:
            # Signed derived figures are carried as exact rationals in the result.
            contracts.RATIONAL.validate(format_rational(number), path)
    except ContractError:
        return "unrepresentable"
    return None


def validate_measurements(experiment, doc, registry, root=None):
    schema = jsonsubset.load(registry["experiments"][experiment]["schema_file"], root).doc
    for path, token, node in _numeric_fields(doc, schema, schema.get("$defs", {})):
        reason = _numeric_reason(path, token, node)
        if reason is not None:
            raise MeasurementRefusal(path, token, reason)


def measurement_evidence(error, data):
    token = error.token.encode("ascii")
    return {"document_sha256": sha256_bytes(data), "document_bytes": len(data),
            "field": error.field, "token": error.token[:512], "token_bytes": len(token),
            "token_sha256": sha256_bytes(token), "reason": error.reason}


def measurement_detail(run, evidence):
    return ("%s/round-%d/%s: %s: %s (numeric token %s)" % (
        run["experiment"], run["round"], run["side"], evidence["field"],
        evidence["reason"], evidence["token"]))[:4096]


def verify_measurement_refusal(run, registry, root=None):
    """Replay the bounded numeric projection. It has no document or judgment authority."""
    evidence = run["measurement_refusal"]
    path, token = evidence["field"], evidence["token"]
    require(len(path) <= 256 and re.fullmatch(contracts.TELEMETRY_MEASUREMENT_PATH, path),
            "measurement refusal: a bounded canonical field path")
    # Resolve schema paths directly without allocating candidate-sized arrays. A candidate
    # cannot smuggle arbitrary keys, names, text, or a signed figure into a raw-metric rule.
    schema = jsonsubset.load(registry["experiments"][run["experiment"]]["schema_file"], root).doc
    definitions = schema.get("$defs", {})

    def resolve(node, remaining):
        if "$ref" in node:
            return resolve(definitions[node["$ref"].split("/")[-1]], remaining)
        if "oneOf" in node:
            return [found for child in node["oneOf"] for found in resolve(child, remaining)]
        if not remaining:
            kinds = node.get("type", ())
            return [node] if "number" in kinds or "integer" in kinds else []
        part, rest = remaining[0], remaining[1:]
        if part.startswith("["):
            index = int(part[1:-1])
            if index >= node.get("maxItems", 100000000):
                return []
            child = node.get("items")
        else:
            child = node.get("properties", {}).get(part)
        return [] if child is None else resolve(child, rest)

    parts = re.findall(r"[a-z_][a-z0-9_-]*|\[[0-9]+\]", path)
    nodes = resolve(schema, parts)
    require(len(nodes) == 1, "measurement refusal: field is not a declared numeric field")
    require(evidence["document_bytes"] >= evidence["token_bytes"],
            "measurement refusal: token fits its original document")
    if evidence["token_bytes"] > 512:
        require(len(token) == 512 and evidence["reason"] == "token_too_long"
                and re.fullmatch(r"-?(?:0|[1-9][0-9]*)(?:\.[0-9]*)?(?:[eE][+-]?[0-9]*)?",
                                 token) is not None,
                "measurement refusal: a capped original token")
    else:
        require(len(token) == evidence["token_bytes"]
                and sha256_bytes(token.encode("ascii")) == evidence["token_sha256"],
                "measurement refusal: original numeric token identity")
        try:
            parsed = json.loads(token, parse_int=_NumericToken, parse_float=_NumericToken,
                                parse_constant=_NumericToken)
            require(isinstance(parsed, _NumericToken), "measurement refusal: a numeric token")
            reason = _numeric_reason(path, token, nodes[0])
        except (ValueError, TypeError) as error:
            raise ContractError("measurement refusal: invalid numeric token") from error
        require(reason == evidence["reason"], "measurement refusal: numeric refusal must replay")
    require(run["detail"] == measurement_detail(run, evidence),
            "measurement refusal: diagnostic must match retained evidence")


def read_document(experiment, data, where, registry, root=None):
    """A script's document through the product's committed schema of it."""
    entry = registry["experiments"][experiment]
    require(isinstance(data, bytes) and len(data) <= DOCUMENT_BYTES_MAX,
            "%s: over its document byte bound" % where)
    try:
        # Preserve the original numeric spelling, including non-finite and overflow tokens,
        # only for the fixed numeric projection. The normal strict reader still admits
        # every successful document below; no arbitrary refused document is published.
        numeric = json.loads(data.decode("utf-8"), object_pairs_hook=jsonsubset._unique,
                             parse_int=_NumericToken, parse_float=_NumericToken,
                             parse_constant=_NumericToken)
    except (UnicodeDecodeError, ValueError, RecursionError) as error:
        raise ContractError("%s: not one JSON document: %s" % (where, str(error)[:512]))
    validate_measurements(experiment, numeric, registry, root)
    doc = jsonsubset.parse(data, where, DOCUMENT_BYTES_MAX)
    require(isinstance(doc, dict) and doc.get("schema") == entry["document"],
            "%s: expected %s" % (where, entry["document"]))
    jsonsubset.load(entry["schema_file"], root).validate(doc, where)
    return doc


def expectation(result, run):
    """What the document of `run` must be: the run as `result` planned it."""
    config = result["configuration"]
    expect = dict(config[run["experiment"]], seed=run["seed"],
                  version=result["binaries"][run["side"]]["version"], corpus=None)
    if run["experiment"] == "read_latency":
        corpus = result["corpus"]
        expect["corpus"] = dict(corpus["facts"] or {}, runs=corpus["runs"], seed=corpus["seed"])
    return expect


def _expected_corpus_name(module, runs):
    return "telemetry-corpus-10k" if runs == module.FULL_RUNS else "telemetry-corpus-%d" % runs


def admit(experiment, doc, expect, registry, root=None):
    """(verdict, reasons) of one document read through its schema: refused unless it is the
    planned run (`expectation`) and every figure it derives is what its script's own rule
    gives its raw pairs or samples."""
    validate_measurements(experiment, doc, registry, root)
    module = script_module(registry["experiments"][experiment]["script"], root)
    require(expect["version"] is not None and doc["af"]["version"] == expect["version"],
            "it reports binary %r, not this side's %r" % (doc["af"]["version"][:64],
                                                          expect["version"]))
    if experiment == "read_latency":
        return _admit_read_latency(module, doc, expect)
    protocol_doc = doc["protocol"]
    require((protocol_doc["pairs"], protocol_doc["scale"], protocol_doc["seed"])
            == (expect["pairs"], expect["scale"], expect["seed"]),
            "its protocol (%s pairs, %s, seed %s) is not the planned %d pairs, %s, seed %d" % (
                protocol_doc["pairs"], protocol_doc["scale"], protocol_doc["seed"],
                expect["pairs"], expect["scale"], expect["seed"]))
    if experiment == "overhead":
        names = [fixture["name"] for fixture in doc["fixtures"]]
        require(names == sorted(module.FIXTURES), "its fixtures are not exactly %s" % ", ".join(
            sorted(module.FIXTURES)))
        for fixture in doc["fixtures"]:
            require(len(fixture["pairs"]) == expect["pairs"],
                    "%s holds %d pairs, not %d" % (fixture["name"], len(fixture["pairs"]),
                                                   expect["pairs"]))
        drain = doc["drain"]
        require(drain is not None and drain["fixture"] == module.DRAIN_FIXTURE
                and len(drain["pairs"]) == expect["pairs"],
                "its drain latency is not %d pairs of %s" % (expect["pairs"],
                                                            module.DRAIN_FIXTURE))
    else:
        planned = [(fixture, attempts, size) for fixture, attempts in module.FIXTURES.items()
                   for size in module.SIZES[expect["scale"]]]
        found = [(case["fixture"], case["attempts"], case["tool_output_bytes"])
                 for case in doc["cases"]]
        require(found == planned and [case["name"] for case in doc["cases"]]
                == ["%s %s" % (fixture, module.label(size)) for fixture, _, size in planned],
                "its cases are not exactly the %s scale's" % expect["scale"])
        for case in doc["cases"]:
            for level in module.LEVELS:
                require(len(case["levels"][level]["pairs"]) == expect["pairs"],
                        "%s %s holds %d pairs, not %d" % (
                            case["name"], level, len(case["levels"][level]["pairs"]),
                            expect["pairs"]))
    again = copy.deepcopy(doc)
    try:
        again = module.summarize(again)
    except (module.BenchError, KeyError, TypeError, ValueError, ZeroDivisionError) as error:
        raise ContractError("its script refuses to judge it: %s" % str(error)[:512])
    require(again == doc, "a difference, median, interval or verdict it records is not what "
            "its script's rule gives its pairs")
    validate_figures(experiment, doc)
    return doc["gate"]["verdict"], [str(reason)[:512] for reason in doc["gate"]["reasons"]][:64]


def _admit_read_latency(module, doc, expect):
    corpus = doc["corpus"]
    planned = expect["corpus"]
    require(corpus["runs"] == expect["runs"] == planned["runs"]
            and corpus["seed"] == planned["seed"],
            "it read a corpus of %s runs, seed %s, not the generated %d runs, seed %d" % (
                corpus["runs"], corpus["seed"], planned["runs"], planned["seed"]))
    require(corpus["name"] == _expected_corpus_name(module, corpus["runs"]),
            "its corpus is named %r, not what %d runs are named" % (corpus["name"][:64],
                                                                     corpus["runs"]))
    for name in CORPUS_FACTS:
        require(name in planned and corpus[name] == planned[name],
                "its corpus %s is %s, not the generated corpus's %s" % (
                    name, corpus[name], planned.get(name)))
    for name, expected in module.expected_counts(planned["seed"], expect["runs"]).items():
        require(corpus[name] == expected,
                "its corpus %s is %s, not the generator protocol's %s" % (
                    name, corpus[name], expected))
    names = [item["name"] for item in doc["measurements"]]
    require(names == list(READ_MEASUREMENTS), "its measurements are not exactly " + ", ".join(
        READ_MEASUREMENTS))
    cold = doc["cold"]
    reasons = []
    for item in doc["measurements"]:
        name = item["name"]
        require(item["bound_ms"] == module.BOUNDS_MS[name],
                "%s: a bound of %s ms, not the script's %s ms" % (name, item["bound_ms"],
                                                                   module.BOUNDS_MS[name]))
        warm = item["warm"]
        repeats = 1 if name == "fresh_status" else expect["repeat"]
        samples = warm["samples_ms"]
        require(len(samples) == repeats, "%s: %d samples, not the protocol's %d" % (
            name, len(samples), repeats))
        require(warm["latency_ms"] == sorted(samples)[(len(samples) - 1) // 2],
                "%s: its latency is not the median of its samples" % name)
        if cold["available"]:
            entry = item["cold"]
            require(entry is not None and len(entry["samples_ms"]) == 1
                    and entry["latency_ms"] == entry["samples_ms"][0],
                    "%s: its cold figure is not its one sample" % name)
        else:
            require(item["cold"] is None, "%s: a cold figure where none was measured" % name)
        within = warm["latency_ms"] <= module.BOUNDS_MS[name]
        require(item["within_bound"] is within, "%s: within_bound is not its latency against "
                "its bound" % name)
        if not within:
            reasons.append("%s: %s ms warm, bound %s ms" % (name, warm["latency_ms"],
                                                           module.BOUNDS_MS[name]))
    require(doc["passed"] is (not reasons), "its passed is not its measurements'")
    validate_figures("read_latency", doc)
    if corpus["runs"] != module.FULL_RUNS:
        return "not_judged", ["corpus %s of %d runs: the read bounds are stated for the full "
                              "%d-run corpus" % (corpus["name"], corpus["runs"],
                                                 module.FULL_RUNS)]
    return ("fail" if reasons else "pass"), reasons


def _number(value, where):
    require(type(value) in (int, float), where + ": not a number")
    return Fraction(repr(value)) if isinstance(value, float) else Fraction(value)


def _median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def figures(experiment, doc):
    """{(subject, level, metric, shape): median value} of one document, and its within-binary
    figures [(subject, level, figure, value)], every value an exact Fraction."""
    values, within = {}, []
    if experiment == "overhead":
        for fixture in doc["fixtures"]:
            name = fixture["name"]
            for level in ("off", "spans"):
                for metric in ("wall_ms", "peak_rss_bytes"):
                    values[(name, level, metric, None)] = _median(
                        [_number(pair[level][metric], name) for pair in fixture["pairs"]])
            within.append((name, "spans", "overhead_median_pct",
                           _number(fixture["median_pct"], name)))
            within.append((name, "spans", "overhead_upper_pct",
                           _number(fixture["interval_pct"][1], name)))
        drain = doc["drain"]
        if drain is not None:
            name = drain["fixture"] + " drain"
            for level in ("off", "spans"):
                values[(name, level, "drain_ms", None)] = _median(
                    [_number(pair[level + "_ms"], name) for pair in drain["pairs"]])
            within.append((name, "spans", "drain_median_ms", _number(drain["median_ms"], name)))
    elif experiment == "invocation_cost":
        for case in doc["cases"]:
            name = case["name"]
            for level in ("off", "spans"):
                entry = case["levels"][level]
                for shape in SHAPES:
                    for metric in ("wall_ms", "peak_rss_bytes"):
                        values[(name, level, metric, shape)] = _median(
                            [_number(pair[shape][metric], name) for pair in entry["pairs"]])
                within.append((name, level, "wall_median_pct",
                               _number(entry["wall_median_pct"], name)))
                within.append((name, level, "wall_upper_pct",
                               _number(entry["wall_interval_pct"][1], name)))
                within.append((name, level, "rss_median_bytes",
                               _number(entry["rss_median_bytes"], name)))
    else:
        for item in doc["measurements"]:
            name = item["name"]
            values[(name, "content", "latency_ms", None)] = _number(item["warm"]["latency_ms"],
                                                                   name)
            values[(name, "content", "peak_rss_bytes", None)] = _number(
                item["warm"]["peak_rss_bytes"], name)
            within.append((name, "content", "latency_ms",
                           _number(item["warm"]["latency_ms"], name)))
            within.append((name, "content", "index_bytes",
                           _number(item["warm"]["index_bytes"], name)))
    return values, within


def validate_figures(experiment, doc):
    """Admit output representability per run, before it can enter finalization.

    Even representable raw samples can have an exact median one decimal place longer.
    Such a refusal belongs to this run and must not abort other runs' result publication.
    """
    values, within = figures(experiment, doc)
    for (subject, level, metric, shape), value in values.items():
        where = "%s/%s/%s/%s: derived physical median" % (subject, level, shape or "", metric)
        contracts.DECIMAL.validate(format_decimal(value, where), where)
    for subject, level, figure, value in within:
        contracts.RATIONAL.validate(format_rational(value), "%s/%s/%s" % (
            subject, level, figure))


def protocol(experiment, doc):
    """What two documents must share for their figures to be set side by side: the protocol
    (pairs, scale, seed, bounds), the corpus for read latency, and the machine's kind."""
    machine = dict((key, doc["machine"][key]) for key in ("os", "arch", "cpus"))
    if experiment == "read_latency":
        corpus = dict((key, value) for key, value in doc["corpus"].items() if key != "dir")
        return {"corpus": corpus, "cold": doc["cold"]["available"], "machine": machine}
    return {"protocol": doc["protocol"], "machine": machine}


def derive(result, documents):
    """`within` and `comparisons` of a result from its runs' documents alone (`documents`:
    {(experiment, round, side): parsed document}); the reader derives them again."""
    within = []
    keyed = {}
    for run in result["runs"]:
        key = (run["experiment"], run["round"], run["side"])
        if key not in documents:
            continue
        values, side_within = figures(run["experiment"], documents[key])
        for subject, level, figure, value in side_within:
            within.append({"experiment": run["experiment"], "round": run["round"],
                           "side": run["side"], "subject": subject[:256], "level": level,
                           "figure": figure, "value": format_rational(value)})
        for (subject, level, metric, shape), value in values.items():
            keyed.setdefault((run["experiment"], subject[:256], level, metric, shape), {})[
                (run["round"], run["side"])] = value
    comparisons = []
    rounds = result["configuration"]["rounds"]
    same_host = result["host"]["same_host"]
    for key in sorted(keyed, key=lambda item: (contracts.TELEMETRY_EXPERIMENTS.index(item[0]),
                                               item[1], item[2], item[3], item[4] or "")):
        experiment, subject, level, metric, shape = key
        found = keyed[key]
        entries, notes = [], []
        for index in range(rounds):
            pair = [(experiment, index, side) for side in SIDES]
            if not all(item in documents for item in pair):
                notes.append("round %d: %s" % (index, ", ".join(
                    "the %s run did not succeed" % side for (_, _, side) in pair
                    if (experiment, index, side) not in documents)))
                continue
            if protocol(experiment, documents[pair[0]]) != protocol(experiment,
                                                                    documents[pair[1]]):
                notes.append("round %d: the two documents' protocol, corpus or machine "
                             "differ" % index)
                continue
            if (index, SIDES[0]) not in found or (index, SIDES[1]) not in found:
                notes.append("round %d: one side's document holds no %s" % (index, subject))
                continue
            base, cand = found[(index, SIDES[0])], found[(index, SIDES[1])]
            if base <= 0:
                notes.append("round %d: a baseline of zero has no relative change" % index)
                continue
            entries.append({"round": index, "baseline": format_decimal(base),
                            "candidate": format_decimal(cand),
                            "change": format_rational((cand - base) / base)})
        if not same_host:
            state, reason, entries = "incompatible", ("the host was not observed the same at "
                                                      "both ends of the run, so no same-host "
                                                      "comparison is made"), []
        elif entries:
            state, reason = "compared", "; ".join(notes)[:4096]
        elif any("protocol" in note or "holds no" in note for note in notes):
            state, reason = "incompatible", "; ".join(notes)[:4096]
        else:
            state, reason = "missing", ("; ".join(notes) or "no round has both sides")[:4096]
        median = (format_rational(_median([Fraction(entry["change"]) for entry in entries]))
                  if state == "compared" else None)
        comparisons.append({"experiment": experiment, "subject": subject, "level": level,
                            "metric": metric, "shape": shape, "state": state, "reason": reason,
                            "rounds": entries, "median_change": median})
    return within, comparisons[:1024]


# A run ---------------------------------------------------------------------------------------

class Settings(object):
    def __init__(self, **values):
        self.profile = None
        self.trigger = None
        self.baseline_af = None
        self.candidate_af = None
        self.baseline_sha = None
        self.candidate_sha = None
        self.authority_sha = None
        self.experiments = None
        self.rounds = None
        self.pairs = None
        self.scale = None
        self.corpus_runs = None
        self.output = None
        self.work = None
        self.result_id = None
        self.root = None
        # The explicitly selected execution mode (`telemetryexec.MODES`); nothing is implied.
        self.mode = None
        self.python = sys.executable
        # The contained mode's container runtime, sample image pinned by digest, dedicated
        # disk-bounded scratch filesystem and container user.
        self.docker = "docker"
        self.image = None
        self.scratch_fs = None
        self.scratch_owner = None
        self.uid = os.getuid()
        self.gid = os.getgid()
        # Each side's build record (`af.perf-telemetry-build/1`, as parsed), or None.
        self.builds = {"baseline": None, "candidate": None}
        self.build_paths = {}
        self.started = None
        # The machine lock's directory (tests only); the system's /tmp otherwise.
        self.lock_directory = None
        # Test hooks: how the contained host is checked (a callable (docker, image, scratch_fs,
        # environment, budget, clock) -> (scratch root, CPUs, runtime identity)), how the image
        # is identified (a callable (docker, reference) -> an object with `reference` and
        # `record()`), and an executor to use instead of the mode's own.
        self.contained_host = None
        self.image_identity = None
        self.executor = None
        self.clock = time.monotonic
        self.wall = time.time
        self.host_identity = host.identity
        self.free_bytes = lambda path: shutil.disk_usage(path).free
        for key, value in values.items():
            require(hasattr(self, key), "unknown telemetry-run setting " + key)
            setattr(self, key, value)


def _sha(value, name):
    require(isinstance(value, str) and len(value) == 40
            and all(char in "0123456789abcdef" for char in value),
            "%s: a full lowercase commit SHA" % name)
    return value


BINARY_BYTES_MAX = 512 * 1024 * 1024


def _unchanged(fd, before, path):
    def identity(info):
        return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)
    require(identity(before) == identity(os.fstat(fd)) == identity(os.lstat(path)),
            "%s changed while it was being admitted" % path)


def _file_sha256(path, budget=None):
    path = os.path.abspath(path)
    fd, info = admission.open_confined(os.path.sep, path.lstrip(os.path.sep), path)
    try:
        require(info.st_size <= BINARY_BYTES_MAX, "%s exceeds the binary byte bound" % path)
        digest = admission.read_bounded(fd, info.st_size, budget, path, "binary hashing")
        _unchanged(fd, info, path)
        return digest
    finally:
        os.close(fd)


def _sync(budget, step, function, *args, **kwargs):
    with budget.enforce(step):
        return function(*args, **kwargs)


def _binary(path, name, budget=None):
    require(path and os.path.isfile(path) and not os.path.islink(path)
            and os.access(path, os.X_OK), "%s: an executable AF binary, not a link" % name)
    return os.path.abspath(path), _file_sha256(path, budget)


def read_build(path, side, source_sha, binary_sha256, budget=None):
    """A side's build record (`af.perf-telemetry-build/1`), bound to that side's source and
    binary: refused when it names another."""
    absolute = os.path.abspath(path)
    fd, info = admission.open_confined(os.path.sep, absolute.lstrip(os.path.sep), path)
    try:
        require(info.st_size <= contracts.limits.MAX_JSON_BYTES, "build record exceeds its byte bound")
        chunks = []
        admission.read_bounded(fd, info.st_size, budget, path, "build record admission", chunks.append)
        _unchanged(fd, info, absolute)
        doc = contracts.read_json(b"".join(chunks), "telemetry-build", path)
    finally:
        os.close(fd)
    require(doc["side"] == side and doc["source_sha"] == source_sha
            and doc["binary_sha256"] == binary_sha256,
            "%s: the build record of %s %s binary %s, not this %s's" % (
                path, doc["side"], doc["source_sha"][:12], doc["binary_sha256"], side))
    return doc


def _write_new(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "xb") as stream:
        stream.write(data)


def _log(output, relative, outcome_stderr, total):
    data = outcome_stderr
    path = os.path.join(output, relative)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "xb") as stream:
        stream.write(data)
    return {"path": relative, "sha256": sha256_bytes(data), "size": len(data),
            "truncated": total > len(data)}


def private_paths(run_dir):
    """A run directory's private HOME and the environment of an execution in it."""
    home = os.path.join(run_dir, "home")
    return home, private_environment(home)


def contained_environment(registry, config):
    """The container limits of a contained result: the registry's, with the profile's work
    bytes as the scratch filesystem's bound."""
    limits_doc = registry["containment"]
    return {"logical_cpus": limits_doc["logical_cpus"],
            "memory_limit_bytes": limits_doc["memory_limit_bytes"],
            "pid_limit": limits_doc["pid_limit"],
            "scratch_limit_bytes": config["work_bytes"], "swap": "disabled",
            "network": "disabled", "cpu_quota": "none"}


class _Preparation:
    """Invocation-local evidence, never inferred from the type of a refusal.

    Unknown/injected setup is conservative. Only the built-in read-only preparation
    establishes no work; entering a host enforcement probe withdraws that evidence.
    """
    no_work = False

    def may_work(self):
        self.no_work = False


def _executor(settings, registry, config, budget, preparation):
    """(executor, the directory every execution's state lives below)."""
    if settings.executor is not None:
        work = os.path.abspath(settings.work or settings.output + ".work")
        if settings.executor.mode == "linux_container":
            work = settings.executor.directory
        return settings.executor, work
    preparation.no_work = True
    if settings.mode == "trusted_local":
        return (telemetryexec.TrustedLocal(settings.python, settings.clock),
                os.path.abspath(settings.work or settings.output + ".work"))
    from . import builds, runner
    from . import budget as deadlines
    require(settings.work is None, "--work: a contained run works on the scratch filesystem")
    require(settings.uid == os.geteuid(), "--mode linux_container runs its containers as this "
            "user (uid %d), so the run can verify and remove what they wrote" % os.geteuid())
    environment = contained_environment(registry, config)
    try:
        if settings.image_identity is not None:
            preparation.may_work()
        image = (settings.image_identity(settings.docker, settings.image)
                 if settings.image_identity else builds.BuildImage(settings.docker, settings.image,
                                                                   budget=budget))
        args = (settings.docker, settings.image, settings.scratch_fs,
                environment, budget, settings.clock)
        if settings.contained_host is not None:
            preparation.may_work()
            scratch_root, cpus, runtime = settings.contained_host(*args)
        else:
            scratch_root, cpus, runtime = runner.contained_host(
                *args, before_probe=preparation.may_work)
    except (builds.Unavailable, runner.Unavailable) as refused:
        raise Unavailable(refused.reason, str(refused))
    except deadlines.Expired as expired:
        raise Unavailable("telemetry_deadline", str(expired))
    executor = telemetryexec.Contained(settings.docker, image, environment, scratch_root,
                                       settings.uid, settings.gid, cpus,
                                       settings.result_id or "telemetry", runtime,
                                       settings.clock, owner=settings.scratch_owner)
    return executor, executor.directory


def run(settings, machine_lease=None):
    """One telemetry experiment run; the result document (also written, with its raw documents,
    logs, report and manifest, below the new `settings.output`). The machine's performance lock
    is held throughout."""
    from . import runner
    started = settings.clock() if settings.started is None else settings.started
    root = settings.root or ROOT
    # Registry loading is bounded too; its profile duration is anchored to entry, never now.
    bootstrap = deadlines.Budget(started + 60, settings.clock, 0)
    registry, registry_sha = _sync(bootstrap, "reading telemetry registry", load_registry, root, bootstrap)
    config = configuration(registry, settings.profile, settings.experiments, settings.rounds,
                           settings.pairs, settings.scale, settings.corpus_runs)
    require(settings.trigger in ("pr", "main_push", "nightly", "release", "manual"),
            "--trigger: pr, main_push, nightly, release or manual")
    mode = settings.executor.mode if settings.executor is not None else settings.mode
    require(mode in telemetryexec.MODES, "--mode: %s, selected explicitly" % " or ".join(
        telemetryexec.MODES))
    require(settings.trigger != "pr" or mode == "linux_container", "a pull request's candidate "
            "is measured only in linux_container mode")
    sources = {"baseline_sha": _sha(settings.baseline_sha, "--baseline-sha"),
               "candidate_sha": _sha(settings.candidate_sha, "--candidate-sha"),
               "authority_sha": _sha(settings.authority_sha, "--authority-sha")}
    deadline = started + config["timeout_minutes"] * 60
    budget = deadlines.Budget(deadline, settings.clock, RESERVE_SECONDS)
    binaries, builds_doc = {}, {}
    with budget.enforce("admitting telemetry inputs"):
        for side, path in (("baseline", settings.baseline_af), ("candidate", settings.candidate_af)):
            binaries[side] = _binary(path, "--%s-af" % side, budget)
            record = (settings.builds or {}).get(side)
            if settings.build_paths.get(side):
                record = read_build(settings.build_paths[side], side, sources[side + "_sha"],
                                    binaries[side][1], budget)
            if record is not None:
                require(record["side"] == side and record["source_sha"] == sources[side + "_sha"]
                        and record["binary_sha256"] == binaries[side][1],
                        "the %s build record names another source or binary" % side)
            builds_doc[side] = record
        digest, files = authority_digest(registry, root, budget)
        output = os.path.abspath(settings.output)
        require(not os.path.lexists(output), "--output %s already exists; a result is a new "
                "directory" % output)
    locks = []
    try:
        try:
            _sync(budget, "acquiring telemetry machine lock", runner.machine_lock,
                  settings.lock_directory, register=locks.append, lease=machine_lease)
        except runner.Unavailable as refused:
            raise Unavailable(refused.reason, str(refused))
        if machine_lease is None:
            locks[-1].guard()
        if settings.scratch_owner is not None:
            try:
                _sync(budget, "fencing telemetry work", settings.scratch_owner.prepare_work)
            except BaseException:
                # Only controller metadata was attempted: no executor preparation, scratch
                # probe or launch has begun. The durable owner still decides scratch cleanup.
                if machine_lease is None:
                    locks[-1].confirm_clean()
                raise
        preparation = _Preparation()
        executor, work = None, settings.scratch_fs or settings.work or settings.output + ".work"
        try:
            executor, work = _sync(budget, "preparing telemetry executor",
                                   _executor, settings, registry, config, budget, preparation)
            # From here controller setup can write even before the first candidate launch.
            preparation.may_work()
            result = _run_locked(settings, root, registry, registry_sha, config, mode, sources,
                               binaries, builds_doc, digest, files, output, deadline, executor,
                               work)
            if result["execution"]["cleanup"] != "confirmed":
                if machine_lease is not None:
                    locks[-1].mark_uncertain()
            elif machine_lease is None:
                locks[-1].confirm_clean()
            return result
        except BaseException as error:
            if executor is not None:
                _uncertain(settings, executor)
            if preparation.no_work and settings.scratch_owner is not None:
                try:
                    _sync(deadlines.Budget(deadline, settings.clock, 0),
                          "settling unused telemetry work", settings.scratch_owner.work_cleaned)
                except BaseException as settlement:
                    add_note(error, "Unused work fence settlement unconfirmed: %s" % settlement)
            if preparation.no_work and machine_lease is None:
                locks[-1].confirm_clean()
            recovery = "Recovery work: %s" % work
            if settings.scratch_owner is not None:
                recovery += "; ownership: %s; mount: %s" % (
                    settings.scratch_owner.path, settings.scratch_owner.mount)
            add_note(error, recovery)
            # A torn terminal write is never an admitted complete bundle.
            terminal = deadlines.Budget(deadline, settings.clock, 0)
            def rollback():
                for name in (bundle.MANIFEST, RESULT_JSON):
                    try:
                        os.unlink(os.path.join(output, name))
                    except FileNotFoundError:
                        pass
                if os.path.isdir(output):
                    _write_new(os.path.join(output, "failure.txt"),
                        ("Telemetry did not complete: %s\n%s\n" %
                         (str(error)[:2048], recovery)).encode())
            if executor is not None:
                try:
                    _sync(terminal, "withdrawing interrupted terminal publication", rollback)
                except BaseException:
                    # Rollback cannot replace the original failure or start a new allowance.
                    pass
            if not preparation.no_work and machine_lease is None and settings.scratch_owner is not None:
                def stopped_and_removed():
                    # Preserve the hosted cleanup path after a publication-only failure.
                    # This is original durable ownership evidence, never a result boolean.
                    ownership = settings.scratch_owner.available()
                    require(not ownership["filesystem_pending"], "scratch cleanup receipt remains pending")
                    require(not os.path.lexists(work), "telemetry work remains; retain machine fence")
                    locks[-1].confirm_clean()
                try:
                    _sync(terminal, "confirming stopped telemetry ownership", stopped_and_removed)
                except BaseException as settlement:
                    add_note(error, "Machine cleanup remains unconfirmed: %s" % settlement)
            raise
    finally:
        for lock in locks:
            lock.release()


class _Guard(object):
    """What no execution may change: both binaries, the measurement authority, the result
    directory beyond what the controller wrote, and the master corpus."""

    def __init__(self, registry, root, digest, binaries, output, budget=None):
        self.registry = registry
        self.budget = budget
        self.root = root
        self.digest = digest
        self.binaries = binaries
        self.output = output
        self.written = inventory(output)
        self.corpus = None
        self.corpus_seen = None

    def wrote(self):
        self.written = inventory(self.output)

    def keep_corpus(self, path):
        self.corpus = path
        self.corpus_seen = inventory(path)

    def problems(self):
        found = []
        for side, (path, expected) in sorted(self.binaries.items()):
            try:
                if os.path.islink(path) or _file_sha256(path, self.budget) != expected:
                    found.append("the %s binary changed" % side)
            except OSError as error:
                found.append("the %s binary cannot be read: %s" % (side, error.strerror))
        try:
            if authority_digest(self.registry, self.root, self.budget)[0] != self.digest:
                found.append("the measurement authority changed")
        except (ContractError, OSError) as error:
            found.append("the measurement authority cannot be read: %s" % error)
        try:
            if inventory(self.output) != self.written:
                found.append("the result directory holds what the controller did not write")
        except OSError as error:
            found.append("the result directory cannot be listed: %s" % error)
        if self.corpus is not None:
            try:
                if inventory(self.corpus) != self.corpus_seen:
                    found.append("the generated corpus changed")
            except OSError as error:
                found.append("the generated corpus cannot be listed: %s" % error)
        return found


def _readable(registry, root, experiment, af_path):
    """The read-only paths of one execution: its experiment's declared inputs and its side's
    binary, never anything else of the checkout."""
    paths = [os.path.join(root, item) for item in registry["experiments"][experiment]["inputs"]]
    return sorted(set(os.path.abspath(path) for path in paths)) + ([af_path] if af_path else [])


def _uncertain(settings, executor):
    # The durable work fence predates setup and every launch. Failure handlers do no
    # compensating ownership I/O, including after the absolute timer has expired.
    executor.pending = True


def _remove_owned(path, settings, executor, state, budget, work):
    deleted = False
    def remove():
        nonlocal deleted
        owner = settings.scratch_owner
        fenced = owner is not None and owner.doc["launches"] > 0
        if owner is not None:
            require(owner.doc["phase"] in ("ready", "stopped"),
                    "scratch filesystem cleanup has unsettled ownership")
        if fenced:
            owner.uncertain()  # Durable before a deletion can fail, stall or be cancelled.
        shutil.rmtree(path)
        require(not os.path.lexists(path), "scratch deletion left its path present")
        deleted = True
        if fenced:
            owner.filesystem_cleaned()
        if owner is not None and path == work:
            owner.work_cleaned()
    try:
        _sync(budget, "removing telemetry scratch", remove)
    except (OSError, ContractError, deadlines.Interrupted) as error:
        owner = settings.scratch_owner
        retained = owner.mount if deleted and path == work and owner is not None else work
        state.update(cleanup="uncertain", retained=retained,
                     stopped=("filesystem cleanup settlement unconfirmed at %s%s: %s" % (
                         path, " (directory removed)" if deleted else "", error))[:4096])
        state["problems"].append(state["stopped"][:512])
        _uncertain(settings, executor)
        return False
    return True


def _run_locked(settings, root, registry, registry_sha, config, mode, sources, binaries,
                builds_doc, digest, files, output, deadline, executor, work):
    budget = deadlines.Budget(deadline, settings.clock, RESERVE_SECONDS)
    require(not _sync(budget, "checking retained telemetry work", os.path.lexists, work),
            "the work directory %s already exists; it may be kept by an earlier run whose "
            "cleanup was not confirmed, and the operator removes it" % work)
    require(not (work + os.sep).startswith(output + os.sep) and work != output
            and not (output + os.sep).startswith(work + os.sep),
            "the work directory lies apart from the result")
    terminal = deadlines.Budget(deadline, settings.clock, 0)
    budget.seconds("initializing telemetry work")

    def initialize():
        os.makedirs(output, 0o755)
        os.makedirs(work, 0o700)
        capture = os.path.join(work, "capture")
        os.mkdir(capture, 0o700)
        runs_dir = os.path.join(work, "runs")
        os.mkdir(runs_dir, 0o700)
        file_bytes = min(FILE_BYTES_MAX, config["work_bytes"])
        started_ms = int(settings.wall() * 1000)
        start_host = settings.host_identity()
        guard = _Guard(registry, root, digest,
                       dict((side, binaries[side]) for side in SIDES), output, budget)
        state = {"stopped": None, "cleanup": "confirmed", "retained": None, "problems": [],
                 "executions": 0}

        return capture, runs_dir, file_bytes, started_ms, start_host, guard, state
    capture, runs_dir, file_bytes, started_ms, start_host, guard, state = _sync(
        budget, "initializing telemetry work", initialize)

    def remaining():
        return deadline - settings.clock()

    def stop_reason():
        if state["stopped"] is not None:
            return state["stopped"]
        if remaining() < RESERVE_SECONDS:
            state["stopped"] = ("the run's %d-minute deadline leaves no time beyond the %d s "
                                "kept for teardown and the result" % (config["timeout_minutes"],
                                                                      RESERVE_SECONDS))
        elif executor.mode == "trusted_local" \
                and _sync(budget, "checking telemetry free space", settings.free_bytes, work) < config["work_bytes"]:
            # A contained run's scratch filesystem is itself the work bound, verified by its
            # enforcement fill; a trusted local run needs the room free on the host.
            state["stopped"] = ("the work filesystem has less free space than the profile's %d "
                                "work bytes" % config["work_bytes"])
        return state["stopped"]

    def execute(argv, run_dir, readable, room, prepare=None, writable=()):
        """One execution in its own new directory `run_dir`, which `prepare` may fill first,
        writing only there and in `writable`; (outcome, integrity problems)."""
        state["executions"] += 1

        def prepare_run():
            os.mkdir(run_dir, 0o700)
            home, env = private_paths(run_dir)
            os.mkdir(home, 0o700)
            if prepare is not None:
                prepare(run_dir)
            return env
        try:
            env = _sync(budget, "preparing telemetry execution and corpus copy", prepare_run)
            budget.seconds("launching telemetry execution")
        except (deadlines.Expired, deadlines.Interrupted) as error:
            state["stopped"] = str(error)[:4096]
            outcome = telemetryexec.Outcome(error=state["stopped"], timed_out=True)
        else:
            outcome = _sync(terminal, "executing telemetry and teardown", executor.run,
                telemetryexec.Execution(argv, env, run_dir, [run_dir] + list(writable),
                    readable, capture, "x%04d" % state["executions"], room,
                    deadline - RESERVE_SECONDS, file_bytes, cleanup_deadline=deadline))
        if outcome.cleanup != "confirmed":
            # Whatever it could reach stays for the operator; nothing more runs.
            _uncertain(settings, executor)
            state["cleanup"] = "uncertain"
            state["retained"] = work
            state["stopped"] = ("an execution's cleanup was not confirmed (%s); its work is kept "
                                "at %s for the operator" % (outcome.cleanup_detail, work))[:4096]
        else:
            if not _remove_owned(run_dir, settings, executor, state, terminal, work):
                outcome.cleanup = "uncertain"
                outcome.cleanup_detail = state["stopped"]
        try:
            problems = _sync(budget, "checking execution integrity", guard.problems)
        except (deadlines.Expired, deadlines.Interrupted) as error:
            problems = [str(error)]
        if problems:
            state["problems"] += [problem for problem in problems
                                  if problem not in state["problems"]]
            state["stopped"] = ("protected state changed during an execution: %s; nothing more "
                                "runs" % "; ".join(problems))[:4096]
        return outcome, problems

    versions = {}
    for side in SIDES:
        versions[side] = None
        if stop_reason() is not None:
            continue
        outcome, problems = execute([binaries[side][0], "--version"],
                                    os.path.join(runs_dir, "version-" + side),
                                    [binaries[side][0]], 64 * 1024 * 1024)
        text = outcome.stdout.decode("utf-8", "replace").strip()
        if outcome.failure() is None and outcome.code == 0 and not problems \
                and 0 < len(text) <= 256:
            versions[side] = text
    result = {"schema": contracts.TELEMETRY_RESULT_SCHEMA,
              "result_id": settings.result_id or "tl_" + os.urandom(8).hex(),
              "trigger": settings.trigger, "profile": settings.profile,
              "started_unix_ms": started_ms, "ended_unix_ms": started_ms, "sources": sources,
              "authority": {"digest": digest, "registry_sha256": registry_sha, "files": files},
              "binaries": dict((side, {"source_sha": sources[side + "_sha"],
                                       "sha256": binaries[side][1], "version": versions[side],
                                       "build": builds_doc[side]})
                               for side in SIDES),
              "configuration": config,
              "execution": {"mode": mode, "containment": executor.containment,
                            "container": executor.record(), "work_bytes": config["work_bytes"],
                            "file_bytes": file_bytes, "cleanup": "confirmed", "retained": None,
                            "problems": [], "limitations": executor.limitations},
              "host": {"host_id": contracts.host_id(start_host), "host": start_host,
                       "end_host_id": contracts.host_id(start_host), "same_host": True},
              "corpus": None, "runs": [], "within": [], "comparisons": [],
              "status": "failed", "verdict_effect": "none", "enforcement": "advisory"}
    documents = {}
    corpus_dir = os.path.join(work, "corpus")
    corpus_ok = False
    if "read_latency" in config["experiments"]:
        script = registry["experiments"]["read_latency"]["script"]
        settings_ = config["read_latency"]
        entry = {"status": "not_run", "runs": settings_["runs"], "seed": config["seed"],
                 "wall_ms": None, "log": None, "facts": None, "detail": ""}
        reason = stop_reason()
        if reason is not None:
            entry["detail"] = reason
        else:
            _sync(budget, "creating master corpus", os.mkdir, corpus_dir, 0o700)
            run_dir = os.path.join(runs_dir, "corpus")
            outcome, problems = execute(
                corpus_argv(settings_, config["seed"], os.path.join(root, script), corpus_dir,
                            executor.python), run_dir,
                _readable(registry, root, "read_latency", None), config["work_bytes"],
                writable=[corpus_dir])
            entry["wall_ms"] = format_decimal(Fraction(outcome.wall_ns, 1000000))
            entry["log"] = _sync(terminal, "retaining corpus log", _log, output,
                                 LOGS + "/corpus.log", outcome.stderr, outcome.stderr_bytes)
            _sync(terminal, "inventorying controller output", guard.wrote)
            failure = outcome.failure()
            if outcome.timed_out:
                entry["status"], entry["detail"] = "timeout", "the corpus generator timed out"
            elif failure is not None or outcome.code != 0 or problems:
                entry["status"] = "failed"
                entry["detail"] = ("the corpus generator %s" % (
                    failure or ("exited %s" % outcome.code if outcome.code != 0
                                else "changed protected state")))[:4096]
            else:
                module = _sync(budget, "loading corpus protocol", script_module, script, root)
                try:
                    facts = _sync(budget, "reading corpus facts", corpus_facts, corpus_dir)
                    expected = settings_["runs"] * module.EXECUTIONS_PER_RUN
                    require(facts["segments"] == expected, "it wrote %d segments, not the %d "
                            "of %d runs" % (facts["segments"], expected, settings_["runs"]))
                except (ContractError, OSError) as error:
                    entry["status"] = "failed"
                    entry["detail"] = ("the generated corpus is incomplete: %s" % error)[:4096]
                else:
                    entry["status"], entry["facts"], corpus_ok = "ok", facts, True
                    _sync(budget, "inventorying master corpus", guard.keep_corpus, corpus_dir)
        result["corpus"] = entry
    for index in range(config["rounds"]):
        order = SIDES if index % 2 == 0 else tuple(reversed(SIDES))
        for experiment in config["experiments"]:
            script = registry["experiments"][experiment]["script"]
            settings_ = config[experiment]
            seed = config["seed"] + index
            for position, side in enumerate(order):
                record = {"experiment": experiment, "round": index, "side": side,
                          "position": position, "seed": seed,
                          "argv": argv_for(experiment, settings_, seed, script),
                          "status": "not_run", "exit_code": None, "wall_ms": None,
                          "document": None, "log": None, "measurement_refusal": None,
                          "judgment": None, "detail": ""}
                result["runs"].append(record)
                if experiment == "read_latency" and not corpus_ok:
                    record["detail"] = "the corpus was not generated, so nothing was read"
                    continue
                reason = stop_reason()
                if reason is not None:
                    record["detail"] = reason
                    continue
                if versions[side] is None:
                    record["detail"] = ("the %s binary's --version query did not succeed, so "
                                        "its documents cannot be bound to it" % side)
                    continue
                run_dir = os.path.join(runs_dir, "%s-%d-%s" % (experiment, index, side))
                argv = argv_for(experiment, settings_, seed, os.path.join(root, script),
                                binaries[side][0], os.path.join(run_dir, "work"),
                                os.path.join(run_dir, "corpus"), executor.python)

                def copy_corpus(run_dir):
                    # Every read gets its own copy, counted in its room: an index one side
                    # builds is never another read's, and the master corpus is never bound.
                    admission.copy_tree(corpus_dir, os.path.join(run_dir, "corpus"),
                        admission.Bounds(1000000, 64, file_bytes, config["work_bytes"]), budget)
                outcome, problems = execute(
                    argv, run_dir, _readable(registry, root, experiment, binaries[side][0]),
                    config["work_bytes"],
                    copy_corpus if experiment == "read_latency" else None)
                label = "%s/%s/round-%d-%s" % (LOGS, experiment, index, side)
                record["log"] = _sync(terminal, "retaining experiment log", _log, output,
                                      label + ".log", outcome.stderr, outcome.stderr_bytes)
                record["exit_code"] = outcome.code if outcome.code is not None and \
                    -255 <= outcome.code <= 255 else None
                record["wall_ms"] = format_decimal(Fraction(outcome.wall_ns, 1000000))
                failure = outcome.failure()
                if problems:
                    record["status"] = "failed"
                    record["detail"] = ("protected state changed while it ran: %s"
                                        % "; ".join(problems))[:4096]
                elif outcome.error is not None:
                    record["status"], record["detail"] = "failed", failure[:4096]
                elif outcome.timed_out:
                    record["status"], record["detail"] = "timeout", failure[:4096]
                elif failure is not None:
                    record["status"], record["detail"] = "failed", failure[:4096]
                elif outcome.code not in (0, EXIT_GATE_FAILED):
                    record["status"] = "failed"
                    record["detail"] = "the script exited %s" % outcome.code
                else:
                    relative = "%s/%s/round-%d-%s.json" % (RAW, experiment, index, side)
                    try:
                        doc = _sync(budget, "reading experiment document", read_document,
                                    experiment, outcome.stdout, relative, registry, root)
                        verdict, reasons = _sync(budget, "admitting experiment document",
                            admit, experiment, doc, expectation(
                            dict(result, corpus=result["corpus"]), record), registry, root)
                        # Read latency exits 3 whenever its reads are over their bounds, at
                        # any scale; the paired scripts exactly when their gate fails.
                        failed = (not doc["passed"] if experiment == "read_latency"
                                  else verdict == "fail")
                        require(outcome.code == (EXIT_GATE_FAILED if failed else 0),
                                "the script's exit status is not its verdict's")
                    except MeasurementRefusal as error:
                        record["status"] = "refused_document"
                        record["measurement_refusal"] = measurement_evidence(error, outcome.stdout)
                        record["detail"] = measurement_detail(record, record["measurement_refusal"])
                    except ContractError as error:
                        record["status"] = "refused_document"
                        record["detail"] = str(error)[:4096]
                    else:
                        path = os.path.join(output, relative)
                        _sync(budget, "retaining experiment document", _write_new,
                              path, outcome.stdout)
                        record["document"] = {"path": relative,
                                              "sha256": sha256_bytes(outcome.stdout),
                                              "size": len(outcome.stdout)}
                        record["judgment"] = {"verdict": verdict, "reasons": reasons}
                        record["status"] = "ok"
                        documents[(experiment, index, side)] = doc
                _sync(terminal, "inventorying controller output", guard.wrote)
    if state["cleanup"] == "confirmed":
        _remove_owned(work, settings, executor, state, terminal, work)
    with deadlines.Budget(deadline, settings.clock, 1).enforce("finalizing telemetry result"):
        # A side's binary ran on this host; the scripts, fixtures and schemas that measured it
        # must still be the ones both sides were measured with.
        if authority_digest(registry, root, terminal)[0] != digest \
                and "the measurement authority changed" not in state["problems"]:
            state["problems"].append("the measurement authority changed")
        end_host = settings.host_identity()
        result["host"]["end_host_id"] = contracts.host_id(end_host)
        result["host"]["same_host"] = result["host"]["end_host_id"] == result["host"]["host_id"]
        result["ended_unix_ms"] = max(started_ms, int(settings.wall() * 1000))
        result["execution"].update(cleanup=state["cleanup"], retained=state["retained"],
                                   problems=[problem[:512] for problem in state["problems"]][:64])
        result["within"], result["comparisons"] = derive(result, documents)
        result["status"] = result_status(result)
        contracts.validate_telemetry_result(result)
        write_bundle(output, result)
    return result


def result_status(result):
    """`complete` only when every planned run succeeded on one host, the corpus (when one was
    needed) was generated, every cleanup was confirmed and nothing protected changed;
    `failed` when nothing succeeded; `partial` otherwise."""
    oks = [item["status"] == "ok" for item in result["runs"]]
    execution = result["execution"]
    complete = (result["host"]["same_host"] and all(oks)
                and (result["corpus"] is None or result["corpus"]["status"] == "ok")
                and execution["cleanup"] == "confirmed" and not execution["problems"])
    return "complete" if complete else ("partial" if any(oks) else "failed")


def write_bundle(output, result):
    with open(os.path.join(output, RESULT_JSON), "xb") as stream:
        stream.write(contracts.write_bytes(result, "telemetry-result"))
    with open(os.path.join(output, REPORT_HTML), "xb") as stream:
        stream.write(page(result))
    with open(os.path.join(output, SUMMARY_MD), "xb") as stream:
        stream.write(summary_markdown(result).encode("utf-8"))
    return bundle.write_manifest(output, BUNDLE_BYTES_MAX)


# Verification -------------------------------------------------------------------------------

def _members(result):
    expected = set([RESULT_JSON, REPORT_HTML, SUMMARY_MD])
    if result["corpus"] is not None and result["corpus"]["log"] is not None:
        expected.add(result["corpus"]["log"]["path"])
    for run in result["runs"]:
        for item in (run["log"], run["document"]):
            if item is not None:
                expected.add(item["path"])
    return expected


def read_bundle(source):
    """(result, bundle identity, hashes) of the bundle at `source`: every file rehashed, the
    result's contract checked and every member it names present, nothing else. Nothing is
    judged again; `verify` does that."""
    label = "telemetry result %s" % os.path.basename(os.path.normpath(source))
    identity, hashes = bundle.verify(source, BUNDLE_BYTES_MAX, label=label)
    data = bundle.read_member(source, RESULT_JSON, hashes, contracts.limits.MAX_JSON_BYTES,
                              label)
    result = contracts.read_json(data, "telemetry-result", label + "/" + RESULT_JSON)
    expected = _members(result)
    for run in result["runs"]:
        for item in (run["log"], run["document"]):
            if item is not None:
                require(hashes.get(item["path"]) == item["sha256"],
                        "%s: %s is not the verified member its result names" % (label,
                                                                              item["path"]))
    require(set(hashes) == expected, "%s: holds files its result does not name: %s" % (
        label, ", ".join(sorted(set(hashes) - expected)) or "none missing"))
    return result, identity, hashes


def verify(source, root=None):
    """(result, bundle identity, hashes) of the result bundle at `source`, read back offline
    under exactly the measurement authority it names: the checkout at `root` (default: this
    one) must be that authority, by the content of every file it reads, or the result is
    refused; nothing is ever kept as recorded. Every raw document is read through its schema
    again, admitted again as its planned run and judged again by its script's own rule, and
    the figures, comparisons, page and summary are recomputed; anything that differs is
    refused. Only the authority's own scripts are executed, never anything of the result."""
    root = root or ROOT
    registry, registry_sha = load_registry(root)
    label = "telemetry result %s" % os.path.basename(os.path.normpath(source))
    result, identity, hashes = read_bundle(source)
    digest, files = authority_digest(registry, root)
    installed_registry, installed_registry_sha = load_registry(ROOT)
    installed_digest, installed_files = authority_digest(installed_registry, ROOT)
    require((digest, files, registry_sha)
            == (installed_digest, installed_files, installed_registry_sha),
            "%s: installed verifier does not match the retained measurement authority; "
            "install its trusted verifier before verification" % label)
    authority = result["authority"]
    require((authority["digest"], authority["files"], authority["registry_sha256"])
            == (digest, files, registry_sha),
            "%s: measured under authority %s (%s), not %s at %s; it can only be verified "
            "with a checkout of exactly that authority" % (
                label, authority["digest"], result["sources"]["authority_sha"][:12], digest,
                root))
    documents = {}
    for run in result["runs"]:
        if run["measurement_refusal"] is not None:
            verify_measurement_refusal(run, registry, root)
        if run["document"] is None:
            continue
        where = "%s/%s" % (label, run["document"]["path"])
        doc = read_document(run["experiment"], bundle.read_member(
            source, run["document"]["path"], hashes, DOCUMENT_BYTES_MAX, label), where,
            registry, root)
        try:
            verdict, reasons = admit(run["experiment"], doc, expectation(result, run),
                                     registry, root)
        except ContractError as error:
            raise ContractError("%s: %s" % (where, error))
        require({"verdict": verdict, "reasons": reasons} == run["judgment"],
                "%s: its judgment is not what its script's rule gives it" % where)
        failed = not doc["passed"] if run["experiment"] == "read_latency" else verdict == "fail"
        require(run["exit_code"] == (EXIT_GATE_FAILED if failed else 0),
                "%s: its recorded exit status is not its verdict's" % where)
        documents[(run["experiment"], run["round"], run["side"])] = doc
    within, comparisons = derive(result, documents)
    require(within == result["within"], "%s: its within-binary figures are not its documents'"
            % label)
    require(comparisons == result["comparisons"], "%s: its comparisons are not its documents'"
            % label)
    require(bundle.read_member(source, REPORT_HTML, hashes, BUNDLE_BYTES_MAX, label)
            == page(result), "%s: its page is not the page the renderer draws from it" % label)
    require(bundle.read_member(source, SUMMARY_MD, hashes, BUNDLE_BYTES_MAX, label)
            == summary_markdown(result).encode("utf-8"),
            "%s: its summary is not its result's" % label)
    return result, identity, hashes


def admit_for_upload(source, root=None):
    """(outcome, detail, result or None) of the workflow's result directory `source`:
    `admitted` only for a bundle that verifies under exactly the measurement authority at
    `root` (default: this checkout) and holds no credential-shaped value; otherwise
    `missing_result`, `refused` or `credential_found`, and nothing of it is uploaded."""
    from . import credentials
    if os.path.islink(source) or not os.path.isfile(os.path.join(source, RESULT_JSON)):
        return "missing_result", "no telemetry result was written at %s" % source, None
    try:
        result, _, _ = verify(source, root)
    except (ContractError, OSError) as error:
        return "refused", ("the result does not verify: %s" % error)[:4096], None
    try:
        findings = credentials.tree(source)
    except (ContractError, OSError) as error:
        return "refused", ("the result cannot be scanned for credentials: %s"
                           % error)[:4096], None
    if findings:
        return "credential_found", "; ".join(findings)[:4096], None
    return "admitted", headline(result), result


def authority_names(registry):
    """The checkout paths the measurement authority is read from (files or directories)."""
    names = set([REGISTRY_PATH] + list(OWN_INPUTS))
    for experiment in registry["experiments"].values():
        names.add("schemas/" + experiment["schema_file"])
        for item in experiment["inputs"]:
            names.add(item)
    return sorted(names)


def read_authority(git, sha):
    """Admit the complete bounded regular Git-object closure before creating any path."""
    entries = git.tree_entries(sha, [REGISTRY_PATH])
    require(len(entries) == 1 and entries[0][3] == REGISTRY_PATH
            and entries[0][0] in ("100644", "100755") and entries[0][1] == "blob",
            "commit %s has no regular %s" % (sha[:12], REGISTRY_PATH))
    raw = git.blobs([entries[0][2]], bytes_max=tomlfile.MAX_TOML_BYTES)[entries[0][2]]
    registry = contracts.validate_telemetry_lab(tomlfile.loads(raw, REGISTRY_PATH))
    prefixes = authority_names(registry)
    entries = git.tree_entries(sha, prefixes)
    require(len(entries) <= AUTHORITY_FILES_MAX, "the authority holds over %d files"
            % AUTHORITY_FILES_MAX)
    paths = set()
    for mode, kind, obj, path in entries:
        require(kind == "blob" and mode in ("100644", "100755"),
                "%s at %s is not a regular file" % (path, sha[:12]))
        check_relative_path(path, "an authority file")
        require(path not in paths and "/__pycache__/" not in "/" + path
                and not path.endswith((".pyc", ".pyo")), "duplicate or bytecode authority path")
        require(any(path == prefix or path.startswith(prefix + "/") for prefix in prefixes),
                "foreign authority path")
        paths.add(path)
    require(all(any(path == prefix or path.startswith(prefix + "/") for path in paths)
                for prefix in prefixes), "the authority is missing a declared input")
    require(all(not any(parent in paths for parent in _parents(path)) for path in paths),
            "an authority file is also a directory")
    blobs = git.blobs([obj for _, _, obj, _ in entries], bytes_max=AUTHORITY_BYTES_MAX)
    require(sum(len(blobs[obj]) for _, _, obj, _ in entries) <= AUTHORITY_BYTES_MAX,
            "the authority exceeds its materialized byte bound")
    members = {path: blobs[obj] for _, _, obj, path in entries}
    require(members.get(REGISTRY_PATH) == raw, "the authority registry changed")
    return members


@dataclass(frozen=True, init=False)
class _GitAuthority:
    """One operation's admitted immutable objects, bound to its Git reader and commit.

    Construction always performs full Git admission; no caller-provided member map or
    cross-operation cache can stand in for it. Materialization still owns the write boundary.
    """
    git: object
    repo: str
    sha: str
    members: object
    digest: str

    def __init__(self, git, sha):
        require(re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", sha) is not None,
                "prepared authority needs an immutable commit identity")
        members = read_authority(git, sha)
        for name, value in (("git", git), ("repo", git.repo), ("sha", sha),
                            ("members", MappingProxyType(members)),
                            ("digest", _member_identity(members))):
            object.__setattr__(self, name, value)

    def checked(self, git, sha):
        require(self.git is git and self.repo == git.repo and self.sha == sha,
                "prepared authority belongs to another Git source")
        require(_member_identity(self.members) == self.digest,
                "prepared authority bytes changed")
        return self.members


def _member_identity(members):
    """Authority identity from exact bounded admitted bytes, with the existing domain."""
    require(len(members) <= AUTHORITY_FILES_MAX, "authority file count exceeds its bound")
    total, entries = 0, []
    for path, data in sorted(members.items()):
        check_relative_path(path, "an authority file")
        require(type(data) is bytes and "/__pycache__/" not in "/" + path
                and not path.endswith((".pyc", ".pyo")), "invalid prepared authority member")
        total += len(data)
        require(total <= AUTHORITY_BYTES_MAX, "authority bytes exceed their bound")
        entries.append([path, sha256_bytes(data)])
    return canonical_digest({"schema": "af.perf-telemetry-authority/2", "files": entries})


@dataclass(frozen=True, init=False)
class _AuthorityCopy:
    """An import's exact authority bytes, rechecked against the verified result.

    Filesystem inputs are freshly enumerated and read on every import. Later copies use
    these owned immutable buffers, never a changed source path or a caller's unbound map.
    """
    root: str
    members: object

    def __init__(self, root, budget):
        registry, registry_sha = load_registry(root)
        members = read_files(root, authority_files(registry, root), AUTHORITY_BYTES_MAX, budget)
        require(sha256_bytes(members[REGISTRY_PATH]) == registry_sha,
                "authority registry changed during preparation")
        object.__setattr__(self, "root", os.path.abspath(root))
        object.__setattr__(self, "members", MappingProxyType(members))

    def checked(self, result, root):
        require(self.root == os.path.abspath(root), "prepared authority belongs to another root")
        authority = result["authority"]
        require((_member_identity(self.members), len(self.members),
                 sha256_bytes(self.members[REGISTRY_PATH])) ==
                (authority["digest"], authority["files"], authority["registry_sha256"]),
                "authority changed before retention")
        return self.members


def _parents(path):
    parts = path.split("/")
    return ("/".join(parts[:index]) for index in range(1, len(parts)))


def read_files(root, names, limit, budget=None):
    """Read admitted regular files through confined descriptors, bounded in aggregate."""
    members, total = {}, 0
    for name in sorted(names):
        admission.check(budget, "reading retained inputs")
        fd, info = admission.open_confined(root, name, "retained input")
        try:
            require(info.st_size <= limit - total, "retained inputs exceed their byte bound")
            chunks = []
            admission.read_bounded(fd, info.st_size, budget, name, "reading retained inputs",
                                   sink=chunks.append)
            members[name] = b"".join(chunks)
            total += info.st_size
        finally:
            os.close(fd)
    return members


def _write_members(target, members, mode, parent_mode=0o777):
    """Write already admitted bytes, retaining the original copy paths' creation modes."""
    os.mkdir(target, mode)
    for name, data in sorted(members.items()):
        check_relative_path(name, "an admitted copy member")
        full = os.path.join(target, *name.split("/"))
        os.makedirs(os.path.dirname(full), mode=parent_mode, exist_ok=True)
        with open(full, "xb") as stream:
            stream.write(data)


def materialize_authority(git, sha, target, kept=None, *, _prepared=None):
    require(_prepared is None or type(_prepared) is _GitAuthority,
            "unbound prepared Git authority")
    members = (read_authority(git, sha) if _prepared is None
               else _prepared.checked(git, sha))
    if kept is not None:
        kept.reserve_trees([(target, members)])
    # Collection reads these members first and reserves all coexisting copies together.
    # Standalone materialization outside history retains its bounded authority contract.
    _write_members(target, members, 0o700, 0o700)
    return target


# Rendering ----------------------------------------------------------------------------------

def _pct(text):
    if text is None:
        return ""
    value = Fraction(text) * 100
    return "%+.2f%%" % float(value)


def _decimal(text):
    return "%.3f" % float(Fraction(text))


def _execution_text(execution):
    container = execution["container"]
    text = "%s (%s)" % (execution["mode"], execution["containment"])
    if container is not None:
        text += "; image %s (%s, %s), CPUs %s, %d memory bytes, %d PIDs, %d scratch bytes" % (
            container["image"]["reference"], container["image"]["id"],
            container["image"]["platform"], ",".join(str(cpu) for cpu in container["cpus"]),
            container["memory_limit_bytes"], container["pid_limit"],
            container["scratch_limit_bytes"])
    text += "; %d work bytes per execution while it writes, %d per file; cleanup %s%s" % (
        execution["work_bytes"], execution["file_bytes"], execution["cleanup"],
        "" if execution["retained"] is None else ", work kept at " + execution["retained"])
    if execution["problems"]:
        text += "; protected state changed: " + "; ".join(execution["problems"])
    return text + ". " + execution["limitations"]


def _build_text(build):
    if build is None:
        return "no build record"
    return "%s in %s: %s; %s" % (build["command"], build["image"], build["rustc"],
                                build["cargo"])


def headline(result):
    counts = {}
    for run in result["runs"]:
        counts[run["status"]] = counts.get(run["status"], 0) + 1
    execution = result["execution"]
    return "%s: %s, %d of %d planned runs succeeded, %s%s%s%s" % (
        result["result_id"], result["status"], counts.get("ok", 0), len(result["runs"]),
        execution["mode"],
        "" if result["host"]["same_host"] else "; the host changed, so nothing is compared",
        "" if execution["cleanup"] == "confirmed" else "; a cleanup was not confirmed",
        "" if not execution["problems"] else "; protected state changed")


def page(result):
    """The result's offline page: identities, every run, each side's own judgment and
    figures by level, and the same-host comparisons, every string escaped."""
    sources, binaries = result["sources"], result["binaries"]
    sections = [render.section("Telemetry experiment", render.paragraph(
        "Advisory: telemetry experiments measure Telemetry itself, beside the comparison; they "
        "never contribute a verdict sample, never change the timing verdict and never satisfy "
        "a required check. Each script judged its own document; the comparisons set two "
        "documents of one round side by side only when their protocol, corpus and machine "
        "agree on one host. The telemetry level is a separate row, never folded.", "note"),
        render.paragraph(headline(result), "verdict"),
        render.table(("field", "value"), [
            ("trigger, profile", "%s, %s" % (result["trigger"], result["profile"])),
            ("baseline", "%s, binary %s, %s" % (sources["baseline_sha"],
                                                binaries["baseline"]["sha256"],
                                                binaries["baseline"]["version"] or
                                                "version unknown")),
            ("candidate", "%s, binary %s, %s" % (sources["candidate_sha"],
                                                 binaries["candidate"]["sha256"],
                                                 binaries["candidate"]["version"] or
                                                 "version unknown")),
            ("measurement authority", "%s at %s, %d files" % (
                result["authority"]["digest"], sources["authority_sha"],
                result["authority"]["files"])),
            ("configuration", "%s: seed %d, %d rounds, %s; overhead %s pairs at %s, invocation "
             "cost %s pairs at %s, corpus %s runs read %s times" % (
                 result["configuration"]["digest"], result["configuration"]["seed"],
                 result["configuration"]["rounds"],
                 ", ".join(result["configuration"]["experiments"]),
                 result["configuration"]["overhead"]["pairs"],
                 result["configuration"]["overhead"]["scale"],
                 result["configuration"]["invocation_cost"]["pairs"],
                 result["configuration"]["invocation_cost"]["scale"],
                 result["configuration"]["read_latency"]["runs"],
                 result["configuration"]["read_latency"]["repeat"])),
            ("execution", _execution_text(result["execution"])),
            ("baseline build", _build_text(binaries["baseline"]["build"])),
            ("candidate build", _build_text(binaries["candidate"]["build"])),
            ("host", "%s at the start, %s at the end%s" % (
                result["host"]["host_id"], result["host"]["end_host_id"],
                "" if result["host"]["same_host"] else " (changed)")),
            ("corpus", "none" if result["corpus"] is None else "%s, %d runs, seed %d%s%s" % (
                result["corpus"]["status"], result["corpus"]["runs"], result["corpus"]["seed"],
                "" if result["corpus"]["facts"] is None else "; " + ", ".join(
                    "%s %d" % (name, result["corpus"]["facts"][name]) for name in CORPUS_FACTS),
                (": " + result["corpus"]["detail"]) if result["corpus"]["detail"] else ""))]))]
    sections.append(render.section("Runs", render.table(
        ("experiment", "round", "side", "position", "seed", "status", "exit", "wall ms",
         "judgment", "document", "log", "detail"),
        [(run["experiment"], run["round"], run["side"], run["position"] + 1, run["seed"],
          run["status"], "" if run["exit_code"] is None else run["exit_code"],
          run["wall_ms"] or "",
          "" if run["judgment"] is None else "%s%s" % (
              run["judgment"]["verdict"], (": " + "; ".join(run["judgment"]["reasons"]))
              if run["judgment"]["reasons"] else ""),
          render.Markup(render.link(run["document"]["path"], "document"))
          if run["document"] else "",
          render.Markup(render.link(run["log"]["path"], "log")) if run["log"] else "",
          run["detail"]) for run in result["runs"]], searchable=True),
        render.paragraph("Command (placeholders: {af} the side's binary, {work} the run's "
                         "private work directory, {corpus} the generated corpus): "
                         + "; ".join(sorted(set(" ".join(run["argv"])
                                                for run in result["runs"]))))))
    sections.append(render.section("Within each binary", render.paragraph(
        "Each side's own figures by level: the overhead of spans over off, the drain latency, "
        "json against stream-json at each level, and the read figures, as each script reports "
        "them (percentages are percent)."), render.table(
        ("experiment", "round", "side", "subject", "level", "figure", "value"),
        [(item["experiment"], item["round"], item["side"], item["subject"], item["level"] or "",
          item["figure"], "%.3f" % float(Fraction(item["value"])))
         for item in result["within"]], searchable=True)))
    sections.append(render.section("Baseline against candidate", render.paragraph(
        "Per round, the median of each side's pairs or reads, and the candidate's relative "
        "change; the median change over rounds. Different levels, metrics and shapes are "
        "separate rows."), render.table(
        ("experiment", "subject", "level", "metric", "shape", "state", "rounds", "median change",
         "note"),
        [(item["experiment"], item["subject"], item["level"], item["metric"],
          item["shape"] or "", item["state"],
          "; ".join("%d: %s to %s (%s)" % (entry["round"], _decimal(entry["baseline"]),
                                            _decimal(entry["candidate"]), _pct(entry["change"]))
                    for entry in item["rounds"]),
          _pct(item["median_change"]), item["reason"]) for item in result["comparisons"]],
        searchable=True)))
    return render.page("AF telemetry experiment %s" % result["result_id"], sections)


def summary_markdown(result):
    """The compact job summary: the headline and each spans/off row of the overhead."""
    lines = ["### AF telemetry experiment (advisory): %s" % render.markdown_cell(
        headline(result)), "",
        "| Experiment | Subject | Level | Metric | Shape | State | Median change |",
        "| --- | --- | --- | --- | --- | --- | --- |"]
    for item in result["comparisons"]:
        if item["metric"] not in ("wall_ms", "latency_ms", "drain_ms"):
            continue
        lines.append("| %s |" % " | ".join(render.markdown_cell(cell) for cell in (
            item["experiment"], item["subject"], item["level"], item["metric"],
            item["shape"] or "", item["state"], _pct(item["median_change"]) or "—")))
    verdicts = ["%s round %d %s: %s" % (run["experiment"], run["round"], run["side"],
                                        run["judgment"]["verdict"])
                for run in result["runs"] if run["judgment"] is not None]
    lines += ["", "Each script's own judgment: " + render.markdown_cell(
        "; ".join(verdicts) or "none"), ""]
    refused = [run["detail"] for run in result["runs"]
               if run["measurement_refusal"] is not None]
    if refused:
        lines += ["Refused measurements: " + render.markdown_cell("; ".join(refused)), ""]
    return "\n".join(lines)


# Scheduling one Actions event (the Performance telemetry workflow) --------------------------

TRIGGERS = {"pull_request": ("pr", "pr"), "schedule": ("nightly", "nightly"),
            "release": ("release", "release"), "workflow_dispatch": ("manual", None)}


def schedule(git, event_name, event, inputs, sha):
    """What one Actions event measures, resolved once, from the trusted control's own code:
    {"state": "planned"|"unavailable", "reason", "trigger", "profile", "control_sha",
    "baseline_sha", "candidate_sha", "experiments", "rounds", "pairs", "retention_days",
    "timeout_minutes", "work_bytes"}. A pull request is measured with its base as both its control and its
    baseline, its merge commit only built; the nightly measures the default branch's tip against
    the highest stable release reachable from it; a published release its tag against the
    highest smaller stable release; a dispatch explicit full SHAs or fully qualified refs, its
    control defaulting to its base. A gate branch is never measured, and no event the workflow
    does not name is scheduled."""
    from . import gitrefs
    require(event_name in TRIGGERS, "event %s is not scheduled" % event_name)
    trigger, profile = TRIGGERS[event_name]
    inputs = inputs or {}
    out = {"state": "planned", "reason": "", "trigger": trigger, "profile": profile,
           "control_sha": None, "baseline_sha": None, "candidate_sha": None,
           "experiments": [], "rounds": None, "pairs": None, "retention_days": None,
           "timeout_minutes": None, "work_bytes": None}

    def unavailable(reason):
        out.update(state="unavailable", reason=reason[:4096])
        return out
    try:
        if event_name == "pull_request":
            pull = event["pull_request"]
            if str(pull["head"]["ref"]).startswith("af-gate/"):
                return unavailable("a Remote Check gate branch is never measured")
            base = git.resolve(pull["base"]["sha"], "the pull request's base")
            out.update(control_sha=base, baseline_sha=base,
                       candidate_sha=git.resolve(sha, "the pull request's merge commit"))
        elif event_name == "schedule":
            tip = git.resolve(sha, "the default branch")
            _, baseline = gitrefs.nightly_baseline(git, tip)
            out.update(control_sha=tip, baseline_sha=baseline, candidate_sha=tip)
        elif event_name == "release":
            tag = str(event["release"]["tag_name"])
            tag_sha, _, _, baseline = gitrefs.release_baseline(git, tag)
            require(tag_sha == git.resolve(sha, "the released commit"),
                    "the release event's commit is not its tag's")
            out.update(control_sha=tag_sha, baseline_sha=baseline, candidate_sha=tag_sha)
        else:
            def spelled(name):
                value = str(inputs.get(name) or "")
                require(re.match(r"^(?:[0-9a-f]{40}|refs/(?:heads|tags)/[A-Za-z0-9]"
                                 r"[A-Za-z0-9._/-]*)$", value) is not None and ".." not in value,
                        "%s: a full commit SHA or a fully qualified ref" % name)
                return git.resolve(value, name)
            base, head = spelled("base"), spelled("head")
            control = spelled("control") if inputs.get("control") else base
            profile = str(inputs.get("profile") or "manual")
            require(profile in contracts.TELEMETRY_PROFILES, "profile: one of " + ", ".join(
                contracts.TELEMETRY_PROFILES))
            out.update(control_sha=control, baseline_sha=base, candidate_sha=head,
                       profile=profile)
            experiments = [item for item in str(inputs.get("experiments") or "").split() if item]
            for item in experiments:
                require(item in contracts.TELEMETRY_EXPERIMENTS, "experiments: of " + ", ".join(
                    contracts.TELEMETRY_EXPERIMENTS))
            out["experiments"] = experiments
            for name in ("rounds", "pairs"):
                value = str(inputs.get(name) or "")
                if value:
                    require(re.match(r"^[0-9]{1,2}$", value) is not None,
                            "%s: a whole number" % name)
                    out[name] = int(value)
    except gitrefs.Unavailable as error:
        return unavailable("%s: %s" % (error.reason, error))
    raw = git.read_file(out["control_sha"], REGISTRY_PATH)
    if raw is None:
        return unavailable("the trusted control %s predates the telemetry experiments"
                           % out["control_sha"][:12])
    registry = contracts.validate_telemetry_lab(tomlfile.loads(raw, REGISTRY_PATH))
    chosen = registry["profiles"][out["profile"]]
    out.update(retention_days=chosen["retention_days"],
               timeout_minutes=min(355, chosen["timeout_minutes"] + 5),
               work_bytes=chosen["work_bytes"])
    configuration(registry, out["profile"], out["experiments"] or None, out["rounds"],
                  out["pairs"])
    return out


def schedule_binding(doc, event_name, event, sha, repository, run_id, attempt):
    require(doc["state"] == "planned", "cannot bind an unavailable plan")
    head = event["pull_request"]["head"]["sha"] if event_name == "pull_request" else sha
    return contracts.validate_telemetry_binding({
        "schema": contracts.TELEMETRY_BINDING_SCHEMA, "repository": repository,
        "run_id": run_id, "attempt": attempt, "event": event_name,
        "event_sha": sha, "run_head_sha": head,
        "candidate_sha": doc["candidate_sha"], "baseline_sha": doc["baseline_sha"],
        "control_sha": doc["control_sha"], "profile": doc["profile"]})


def schedule_outputs(doc):
    """The `name=value` lines later jobs read; every value is checked text, never evidence."""
    args = []
    for name in doc["experiments"]:
        args += ["--experiment", name]
    for name in ("rounds", "pairs"):
        if doc[name] is not None:
            args += ["--" + name, str(doc[name])]
    lines = ["planned=%s" % ("true" if doc["state"] == "planned" else "false")]
    if doc["state"] == "planned":
        lines += ["trigger=%s" % doc["trigger"], "profile=%s" % doc["profile"],
                  "control=%s" % doc["control_sha"], "baseline=%s" % doc["baseline_sha"],
                  "candidate=%s" % doc["candidate_sha"],
                  "retention_days=%d" % doc["retention_days"],
                  "timeout_minutes=%d" % doc["timeout_minutes"],
                  "work_bytes=%d" % doc["work_bytes"],
                  "args=%s" % json.dumps(args, separators=(",", ":"))]
    return "".join(line + "\n" for line in lines)


# History ------------------------------------------------------------------------------------

HISTORY_JSON = "history.json"
HISTORY_HTML = "history.html"
RESULTS = "results"
HISTORY_LOCK = "lock"
AUTHORITIES = "authorities"
STAGING_PREFIX = ".staging-"
REPORT_STAGE = ".report-staging"
BINDINGS = "bindings"
BINDINGS_OWNER = b"af.telemetry-bindings/1\n"
REPORT_OWNER = b"af.telemetry-report-staging/1\n"
HISTORY_BYTES_DEFAULT = 32 * 1024 * 1024 * 1024
HISTORY_RESULTS_DEFAULT = 2048


def _image_id(result):
    container = result["execution"]["container"]
    return None if container is None else container["image"]["id"][:256]


def group_id(result):
    return contracts.telemetry_group_id(
        result["configuration"]["digest"], result["authority"]["digest"],
        result["host"]["host_id"], result["execution"]["mode"], _image_id(result))


class HistoryRoot(object):
    """A durable root of verified telemetry results, opened under its one lock: every import,
    prune and history write holds it, so two collectors never interleave."""

    def __init__(self, root, bytes_max=HISTORY_BYTES_DEFAULT,
                 results_max=HISTORY_RESULTS_DEFAULT, retention_days=None, wall=time.time,
                 budget=None, sweep=True):
        self.root = os.path.abspath(root)
        self.bytes_max = bytes_max
        self.results_max = results_max
        self.retention_days = retention_days
        self.wall = wall
        self.fd = None
        self.pruned = []
        self.budget = budget
        self.sweep = sweep

    def __enter__(self):
        import errno
        import fcntl
        require(self.fd is None, "the history root is already open")
        if os.path.lexists(self.root):
            require(os.path.isdir(self.root) and not os.path.islink(self.root),
                    "%s is not a history directory (a link is never followed)" % self.root)
        else:
            os.makedirs(self.root, 0o755)
        try:
            # The integer returned by os.open has no destructor. Latch Python's deadline
            # handler for just this open/publication, then acquire with owned authority.
            with deadlines.defer_alarm():
                self.fd = os.open(os.path.join(self.root, HISTORY_LOCK), os.O_RDWR | os.O_CREAT
                                  | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0), 0o600)
            try:
                fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as error:
                if error.errno in (errno.EWOULDBLOCK, errno.EAGAIN):
                    raise ContractError("another telemetry history command holds %s" % self.root)
                raise
            results = os.path.join(self.root, RESULTS)
            if not os.path.lexists(results):
                self.reserve_paths({}, directories=(results,))
                os.mkdir(results, 0o755)
            require(os.path.isdir(results) and not os.path.islink(results),
                    "%s is not a directory of results" % results)
            authority_dir = os.path.join(self.root, AUTHORITIES)
            if not os.path.lexists(authority_dir):
                self.reserve_paths({}, directories=(authority_dir,))
                os.mkdir(authority_dir, 0o755)
            require(os.path.isdir(authority_dir) and not os.path.islink(authority_dir),
                    "the retained authorities directory must not be a link")
            binding_dir = os.path.join(self.root, BINDINGS)
            if not os.path.lexists(binding_dir):
                self.reserve_paths({os.path.join(binding_dir, "owner"): len(BINDINGS_OWNER)})
                os.mkdir(binding_dir, 0o700)
                with open(os.path.join(binding_dir, "owner"), "xb") as stream:
                    stream.write(BINDINGS_OWNER)
            require(os.path.isdir(binding_dir) and not os.path.islink(binding_dir),
                    "bindings directory must not be a link")
            marker = os.path.join(binding_dir, "owner")
            require(stat.S_ISREG(os.lstat(marker).st_mode), "binding ownership marker must be regular")
            with open(marker, "rb") as stream:
                require(stream.read(len(BINDINGS_OWNER) + 1) == BINDINGS_OWNER,
                        "foreign bindings directory")
            self.report_staging()
            if self.sweep:
                self.cleanup_staging()
        except BaseException:
            self.__exit__()
            raise
        return self

    def __exit__(self, *exc):
        with deadlines.defer_alarm():
            if self.fd is not None:
                try:
                    os.close(self.fd)
                finally:
                    self.fd = None

    def report_staging(self):
        path = os.path.join(self.root, REPORT_STAGE)
        if not os.path.lexists(path):
            self.reserve_paths({os.path.join(path, "owner"): len(REPORT_OWNER)})
            os.mkdir(path, 0o700)
            # An interrupted marker write fails closed on restart; never adopt foreign data.
            with open(os.path.join(path, "owner"), "xb") as stream:
                stream.write(REPORT_OWNER)
        require(os.path.isdir(path) and not os.path.islink(path), "report staging must be owned")
        owner = os.path.join(path, "owner")
        require(stat.S_ISREG(os.lstat(owner).st_mode), "report staging marker must be regular")
        with open(owner, "rb") as stream:
            require(stream.read(len(REPORT_OWNER) + 1) == REPORT_OWNER,
                    "foreign report staging marker")
        for name in os.listdir(path):
            require(name in ("owner", HISTORY_JSON, HISTORY_HTML, "collection.json",
                             "collection-position.json", "collection-incomplete.json"),
                    "foreign report staging entry")
            require(stat.S_ISREG(os.lstat(os.path.join(path, name)).st_mode),
                    "report staging entries must be regular, never links")
        return path

    def binding_path(self, result_id):
        contracts.ID.validate(result_id, "binding result ID")
        return os.path.join(self.root, BINDINGS, result_id + ".json")

    def read_binding(self, result_id):
        path = self.binding_path(result_id)
        require(stat.S_ISREG(os.lstat(path).st_mode), "retained binding must be regular")
        with open(path, "rb") as stream:
            data = stream.read(65537)
        require(len(data) <= 65536, "retained binding exceeds its byte bound")
        return contracts.read_json(data, "telemetry-binding", path)

    def check_result_binding(self, result):
        # Locally named experiments retain their existing offline import contract. The
        # reserved Actions ID spelling always needs its retained original planning receipt.
        match = re.fullmatch(r"tl_([1-9][0-9]{0,15})_([1-9][0-9]{0,3})", result["result_id"])
        if match is None:
            return
        binding = self.read_binding(result["result_id"])
        require((binding["run_id"], binding["attempt"]) == tuple(map(int, match.groups())),
                "retained plan belongs to another run/attempt")
        for key, source in (("candidate_sha", "candidate_sha"), ("baseline_sha", "baseline_sha"),
                            ("control_sha", "authority_sha")):
            require(binding[key] == result["sources"][source], "retained plan source mismatch")
        require(binding["profile"] == result["profile"] and
                {"pull_request": "pr", "schedule": "nightly", "release": "release",
                 "workflow_dispatch": "manual"}[binding["event"]] == result["trigger"],
                "retained plan event/profile mismatch")

    def retain_binding(self, result_id, binding):
        path = self.binding_path(result_id)
        if os.path.lexists(path):
            require(self.read_binding(result_id) == binding, "retained binding changed")
            return
        data = contracts.write_bytes(binding, "telemetry-binding")
        self.reserve_paths({path: len(data)})
        # This is unaccepted until the corresponding verified result is published. A partial
        # write is removed on entry along with every other unreferenced binding.
        with open(path, "xb") as stream:
            stream.write(data)

    def report_allocation(self, size):
        unit = os.statvfs(self.root).f_frsize
        return ((size + unit - 1) // unit) * unit

    def allocation_for(self, files, directories=(), entries=()):
        """Additional allocated peak, including rounded files and directory name growth.

        All paths belong to this locked root. A directory's small linear table fits in
        one allocation unit; larger tables reserve four units per new name for leaf/index
        splits, as well as their rounded name storage. Never use logical byte totals as
        the capacity admission for copies or expanded archives.
        """
        require(self.fd is not None, "allocation admission needs the history lock")
        unit = self.report_allocation(1)
        dirs, children, checked_parents = set(directories), {}, set()
        for path in list(files) + list(dirs) + list(entries):
            path = os.path.abspath(path)
            require(os.path.commonpath([path, self.root]) == self.root and path != self.root,
                    "allocation path escapes history")
            parent = os.path.dirname(path)
            children.setdefault(parent, set()).add(os.path.basename(path))
            while parent != self.root:
                # No writes occur during this locked admission. Each parent's ancestors
                # and name growth need examination once, even for thousands of siblings.
                if parent in checked_parents:
                    break
                checked_parents.add(parent)
                require(not os.path.islink(parent), "allocation parent must not be a link")
                if os.path.lexists(parent):
                    require(os.path.isdir(parent), "allocation parent must be a directory")
                    break
                dirs.add(parent)
                ancestor = os.path.dirname(parent)
                children.setdefault(ancestor, set()).add(os.path.basename(parent))
                parent = ancestor
        total = sum(((size + unit - 1) // unit) * unit for size in files.values())
        for directory in dirs | set(children):
            exists = os.path.lexists(directory)
            if exists:
                require(os.path.isdir(directory) and not os.path.islink(directory),
                        "allocation directory must not be a link")
                names = set(os.listdir(directory))
                allocated = os.lstat(directory).st_blocks * 512
            else:
                names, allocated = set(), 0
            added = children.get(directory, set()) - names
            name_bytes = 24 + sum(32 + len(name.encode("utf-8")) for name in names | added)
            needed = ((name_bytes + unit - 1) // unit) * unit
            if exists and not allocated and needed == unit:
                # Match used_bytes on filesystems whose small directories have no blocks.
                needed = 0
            elif needed > unit:
                needed = max(2 * needed, allocated + 4 * unit * len(added) if exists else 0)
            total += max(0, needed - allocated)
        return total

    def reserve_paths(self, files, directories=(), entries=()):
        size = self.allocation_for(files, directories, entries)
        require(self.used_bytes() + size <= self.bytes_max,
                "allocation would take %s past its %d-byte bound (history byte bound)"
                % (self.root, self.bytes_max))
        return size

    def reserve_trees(self, trees, files=None, entries=()):
        files = dict(files or {})
        directories = []
        for target, members in trees:
            require(not os.path.lexists(target), "staging tree already exists")
            directories.append(target)
            for name, data in members.items():
                check_relative_path(name, "a staged input")
                path = os.path.join(target, *name.split("/"))
                require(path not in files, "duplicate staged input")
                files[path] = len(data)
        return self.reserve_paths(files, directories, entries)

    def cleanup_staging(self):
        # Interrupted staging is never accepted, and is swept under this lock/deadline.
        report = self.report_staging()
        for name in (HISTORY_JSON, HISTORY_HTML, "collection.json", "collection-position.json"):
            path = os.path.join(report, name)
            if os.path.lexists(path):
                os.unlink(path)
        directory = os.path.join(self.root, BINDINGS)
        for name in os.listdir(directory):
            if name == "owner":
                continue
            require(name.endswith(".json"), "foreign retained binding path")
            ident = name[:-5]
            contracts.ID.validate(ident, "retained binding ID")
            path = os.path.join(directory, name)
            require(stat.S_ISREG(os.lstat(path).st_mode), "binding must be regular")
            if not os.path.isdir(os.path.join(self.results, ident)):
                os.unlink(path)
        for directory in (self.results, os.path.join(self.root, AUTHORITIES)):
            for name in os.listdir(directory):
                if name.startswith(STAGING_PREFIX):
                    path = os.path.join(directory, name)
                    require(not os.path.islink(path), "staging must not be a link")
                    shutil.rmtree(path)

    @property
    def results(self):
        return os.path.join(self.root, RESULTS)

    def names(self):
        return sorted(name for name in os.listdir(self.results)
                      if not name.startswith("."))

    def used_bytes(self):
        total = telemetryexec.allocated([self.root])
        # Some filesystems expose directory name-table length but no allocated blocks.
        # Count their allocated blocks (zero), rather than name-table serialization bytes
        # that move between root and staging on replace. File bytes retain allocated()'s
        # fallback for filesystems without block accounting.
        for directory, subdirs, _ in os.walk(self.root, followlinks=False):
            info = os.lstat(directory)
            if not info.st_blocks:
                total -= info.st_size
        return total

    def authority_path(self, result):
        return os.path.join(self.root, AUTHORITIES, result["authority"]["digest"].split(":")[1])

    def retain_authority(self, result, root, *, _prepared=None):
        """Copy only exact trusted authority bytes, never files from the result bundle."""
        require(_prepared is None or type(_prepared) is _AuthorityCopy,
                "unbound prepared retention authority")
        prepared = _AuthorityCopy(root, self.budget) if _prepared is None else _prepared
        members = prepared.checked(result, root)
        registry = contracts.validate_telemetry_lab(tomlfile.loads(
            members[REGISTRY_PATH], REGISTRY_PATH))
        digest = result["authority"]["digest"]
        target = self.authority_path(result)
        if os.path.lexists(target):
            require(not os.path.islink(target), "retained authority must not be a link")
            kept_registry, _ = load_registry(target)
            require(authority_digest(kept_registry, target)[0] == digest,
                    "retained authority bytes changed")
            return
        staging = os.path.join(self.root, AUTHORITIES, STAGING_PREFIX + os.urandom(6).hex())
        self.reserve_trees([(staging, members)], entries=(target,))
        try:
            _write_members(staging, members, 0o777)
            require(authority_digest(registry, staging)[0] == digest,
                    "authority copy does not match its identity")
            os.rename(staging, target)
        finally:
            if (self.budget is None or self.budget.left() > 0) and os.path.lexists(staging):
                shutil.rmtree(staging)

    def prune_authorities(self):
        referenced = set()
        for name in self.names():
            try:
                result, _, _ = read_bundle(os.path.join(self.results, name))
            except (ContractError, OSError):
                # An unreadable kept bundle may still reference an authority. Preserve
                # those bytes rather than deleting evidence on an uncertain projection.
                return
            referenced.add(os.path.basename(self.authority_path(result)))
        directory = os.path.join(self.root, AUTHORITIES)
        for name in os.listdir(directory):
            if name not in referenced:
                path = os.path.join(directory, name)
                require(not os.path.islink(path), "retained authority must not be a link")
                shutil.rmtree(path)

    def remove(self, name, reason):
        """Remove kept result `name` whole: renamed away first, so a reader never sees half."""
        doomed = os.path.join(self.results, STAGING_PREFIX + "removed-" + os.urandom(6).hex())
        self.reserve_paths({}, entries=(doomed,))
        os.rename(os.path.join(self.results, name), doomed)
        self.pruned.append({"result_id": name, "reason": reason[:4096]})
        shutil.rmtree(doomed)
        path = self.binding_path(name)
        if os.path.lexists(path):
            require(stat.S_ISREG(os.lstat(path).st_mode), "retained binding must be regular")
            os.unlink(path)

    def prune(self):
        """Remove results older than the retention; then, oldest first, results beyond the
        result bound."""
        kept = []
        now_ms = int(self.wall() * 1000)
        for name in self.names():
            try:
                result, _, _ = read_bundle(os.path.join(self.results, name))
            except (ContractError, OSError):
                continue
            kept.append((result["started_unix_ms"], name))
        kept.sort()
        if self.retention_days is not None:
            horizon = now_ms - self.retention_days * 86400 * 1000
            for started, name in list(kept):
                if started < horizon:
                    self.remove(name, "older than the %d-day retention" % self.retention_days)
                    kept.remove((started, name))
        while len(kept) > self.results_max:
            started, name = kept.pop(0)
            self.remove(name, "beyond the %d-result bound, oldest first" % self.results_max)
        self.prune_authorities()

    def import_result(self, source, authority_root=None):
        """Keep the result bundle at `source` below `results/<result id>/`, verified under
        exactly its measurement authority (the checkout at `authority_root`, default this
        one), copied whole and read back, all under the lock; idempotent for the same bundle
        and refused for another under the same ID. Refused when it would take the root past
        its byte or result bounds. Nothing in it is ever executed."""
        result, identity, hashes = verify(source, authority_root)
        target = os.path.join(self.results, result["result_id"])
        if os.path.lexists(target):
            _, existing, _ = read_bundle(target)
            require(existing == identity, "%s already holds another bundle under result %s"
                    % (self.root, result["result_id"]))
            return result, False
        require(len(self.names()) < self.results_max, "%s already keeps its %d results" % (
            self.root, self.results_max))
        members = read_files(source, list(hashes) + [bundle.MANIFEST],
                             BUNDLE_BYTES_MAX + contracts.limits.MAX_JSON_BYTES,
                             self.budget)
        root = authority_root or ROOT
        prepared = _AuthorityCopy(root, self.budget)
        authority_members = prepared.checked(result, root)
        staging = os.path.join(self.results, STAGING_PREFIX + os.urandom(6).hex())
        trees = [(staging, members)]
        authority_target = self.authority_path(result)
        if not os.path.lexists(authority_target):
            trees.append((os.path.join(self.root, AUTHORITIES, STAGING_PREFIX + "0" * 12),
                          authority_members))
        # Both copies coexist with downloaded/expanded inputs and all retained evidence.
        self.reserve_trees(trees, entries=(target, authority_target))
        self.retain_authority(result, root, _prepared=prepared)
        self.reserve_trees([(staging, members)], entries=(target,))
        try:
            _write_members(staging, members, 0o755)
            _, copied, _ = read_bundle(staging)
            require(copied == identity, "the copy of %s is not the verified bundle"
                    % result["result_id"])
            require(self.used_bytes() <= self.bytes_max,
                    "the staged result exceeds the history byte bound")
            if self.budget is not None:
                self.budget.seconds("publishing a verified result")
            os.rename(staging, target)
        finally:
            if (self.budget is None or self.budget.left() > 0) and os.path.lexists(staging):
                shutil.rmtree(staging)
        return result, True

    def write(self, refused=()):
        """Write `history.json` and `history.html` from every result the root keeps; a kept
        result whose bundle no longer reads back is listed as refused, never shown."""
        refused = list(refused)
        results = []
        for name in self.names():
            try:
                result, _, _ = read_bundle(os.path.join(self.results, name))
                result, identity, _ = verify(os.path.join(self.results, name),
                                              self.authority_path(result))
                require(result["result_id"] == name, "is kept under another result's name")
                self.check_result_binding(result)
                results.append((result, identity))
            except (ContractError, OSError) as error:
                refused.append({"input": (RESULTS + "/" + name)[:1024],
                                "reason": str(error)[:4096]})
        stage = self.report_staging()
        require(not any(os.path.lexists(os.path.join(stage, name))
                        for name in (HISTORY_JSON, HISTORY_HTML)),
                "interrupted report staging must be swept before publication")
        old_bytes = 0
        for name in (HISTORY_JSON, HISTORY_HTML):
            path = os.path.join(self.root, name)
            if os.path.lexists(path):
                require(stat.S_ISREG(os.lstat(path).st_mode), "history report must be regular")
                old_bytes += telemetryexec.allocated([path])
        base_bytes = self.used_bytes()
        final_bytes = base_bytes - old_bytes
        # The decimal used_bytes field can affect output length. Resolve that small fixed
        # point before I/O; a failure cannot truncate either complete published report.
        for _ in range(8):
            doc = history_document(results, refused, self.pruned, {
                "bytes_max": self.bytes_max, "results_max": self.results_max,
                "retention_days": self.retention_days}, final_bytes)
            outputs = ((HISTORY_JSON, contracts.write_bytes(doc, "telemetry-history")),
                       (HISTORY_HTML, history_page(doc)))
            allocation = sum(self.report_allocation(len(data)) for _, data in outputs)
            projected = base_bytes - old_bytes + allocation
            if projected == final_bytes:
                break
            final_bytes = projected
        else:
            raise ContractError("history report accounting did not stabilize")
        self.reserve_paths({os.path.join(stage, name): len(data) for name, data in outputs},
                           entries=tuple(os.path.join(self.root, name) for name, _ in outputs))
        # Fixed owned names bound leftovers even if writing or replacement is interrupted.
        # Both writes complete before either existing report is replaced.
        for name, data in outputs:
            with open(os.path.join(stage, name), "xb") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
        require(self.used_bytes() <= self.bytes_max, "staged reports exceed history capacity")
        for name, _ in outputs:
            os.replace(os.path.join(stage, name), os.path.join(self.root, name))
        return doc


def history_document(results, refused, pruned, limits_doc, used_bytes):
    results = sorted(results, key=lambda item: (item[0]["started_unix_ms"],
                                                item[0]["result_id"]))
    groups = {}
    for result, _ in results:
        groups.setdefault(group_id(result), []).append(result)
    doc = {"schema": contracts.TELEMETRY_HISTORY_SCHEMA, "limits": dict(limits_doc),
           "used_bytes": used_bytes, "results": [], "groups": [], "pruned": list(pruned)[:4096],
           "refused": list(refused)[:4096]}
    for result, identity in results:
        doc["results"].append({"result_id": result["result_id"], "bundle_sha256": identity,
                               "authority_path": "%s/%s" % (AUTHORITIES, result[
                                   "authority"]["digest"].split(":")[1]),
                               "page": "%s/%s/%s" % (RESULTS, result["result_id"], REPORT_HTML),
                               "trigger": result["trigger"], "profile": result["profile"],
                               "mode": result["execution"]["mode"],
                               "started_unix_ms": result["started_unix_ms"],
                               "baseline_sha": result["sources"]["baseline_sha"],
                               "candidate_sha": result["sources"]["candidate_sha"],
                               "authority_sha": result["sources"]["authority_sha"],
                               "status": result["status"], "group_id": group_id(result)})
    for key in sorted(groups):
        members = groups[key]
        first = members[0]
        series = {}
        for result in members:
            for item in result["comparisons"]:
                series.setdefault((item["experiment"], item["subject"], item["level"],
                                   item["metric"], item["shape"]), []).append({
                    "result_id": result["result_id"],
                    "candidate_sha": result["sources"]["candidate_sha"],
                    "baseline_sha": result["sources"]["baseline_sha"],
                    "started_unix_ms": result["started_unix_ms"], "state": item["state"],
                    "median_change": item["median_change"]})
        doc["groups"].append({
            "group_id": key, "configuration_digest": first["configuration"]["digest"],
            "authority_digest": first["authority"]["digest"],
            "host_id": first["host"]["host_id"], "mode": first["execution"]["mode"],
            "image_id": _image_id(first),
            "results": [result["result_id"] for result in members],
            "series": [{"experiment": name[0], "subject": name[1], "level": name[2],
                        "metric": name[3], "shape": name[4], "points": points}
                       for name, points in sorted(series.items(), key=lambda item: (
                           item[0][0], item[0][1], item[0][2], item[0][3], item[0][4] or ""))
                       ][:1024]})
    contracts.validate_telemetry_history(doc)
    return doc


def history(root, imports=(), authority_root=None, bytes_max=HISTORY_BYTES_DEFAULT,
            results_max=HISTORY_RESULTS_DEFAULT, retention_days=None, wall=time.time):
    """Under `root`'s lock: prune it to its bounds, import each bundle of `imports` verified
    under the measurement authority at `authority_root`, then write its history.json and
    history.html; refused inputs are listed with their reason."""
    with HistoryRoot(root, bytes_max, results_max, retention_days, wall) as kept:
        kept.prune()
        refused = []
        for source in imports:
            try:
                kept.import_result(source, authority_root)
            except (ContractError, OSError) as error:
                refused.append({"input": os.path.basename(os.path.normpath(source))[:1024],
                                "reason": str(error)[:4096]})
        return kept.write(refused)


def history_page(doc):
    limits_doc = doc["limits"]
    sections = [render.section("Telemetry history", render.paragraph(
        "Advisory. Every result here was verified under exactly the measurement authority it "
        "was measured with. Results are placed in one series only when they share their "
        "configuration, their measurement authority, their execution mode and image and their "
        "host; anything else is a separate group, never a trend."), render.paragraph(
        "This root keeps %d results in %d bytes, within %d results, %d bytes and %s." % (
            len(doc["results"]), doc["used_bytes"], limits_doc["results_max"],
            limits_doc["bytes_max"], "no retention bound" if limits_doc["retention_days"] is None
            else "%d days of retention" % limits_doc["retention_days"])), render.table(
        ("result", "trigger", "profile", "mode", "started (unix ms)", "baseline", "candidate",
         "authority", "status", "group", "page"),
        [(item["result_id"], item["trigger"], item["profile"], item["mode"],
          item["started_unix_ms"], item["baseline_sha"], item["candidate_sha"],
          item["authority_sha"], item["status"], item["group_id"],
          render.Markup(render.link(item["page"], "result")))
         for item in doc["results"]], searchable=True))]
    for group in doc["groups"]:
        sections.append(render.section("Group %s" % group["group_id"], render.paragraph(
            "configuration %s, authority %s, %s%s, host %s: %d results" % (
                group["configuration_digest"], group["authority_digest"], group["mode"],
                "" if group["image_id"] is None else " in image " + group["image_id"],
                group["host_id"], len(group["results"]))), render.table(
            ("experiment", "subject", "level", "metric", "shape", "points"),
            [(series["experiment"], series["subject"], series["level"], series["metric"],
              series["shape"] or "", "; ".join("%s %s: %s" % (
                  point["result_id"], point["candidate_sha"][:12],
                  _pct(point["median_change"]) if point["median_change"] else point["state"])
                  for point in series["points"]))
             for series in group["series"]], searchable=True)))
    sections.append(render.section("Pruned", render.table(
        ("result", "reason"), [(item["result_id"], item["reason"]) for item in doc["pruned"]])
        if doc["pruned"] else render.paragraph("Nothing was pruned by this write.",
                                               "unavailable")))
    sections.append(render.section("Refused", render.table(
        ("input", "reason"), [(item["input"], item["reason"]) for item in doc["refused"]])
        if doc["refused"] else render.paragraph("Nothing was refused.", "unavailable")))
    return render.page("AF telemetry history", sections)
