"""Whole-CLI cases of the laboratory (`fixtures/performance/cli.toml`, ADR-0168).

The authority's CLI registry names, per CLI adapter and per instance of a registry `process`
case, the actual AF binary's argv, the untimed fixture setup before it and the oracle rules its
output must satisfy. This module loads and cross-checks that registry, reads each side's own
package version from the side's Cargo.lock in the plan's Git objects (bound by the plan's
digest, never by the binary's own output), chooses each side's adapter, expands the declared
placeholders and judges an execution. A rule is the authority's, written from the declared
fixture and the product's documented commands; the oracle never learns from candidate output.
"""
import json
import os
import re
import stat

from . import contracts, gitrefs, reference, tomlfile
from . import suite as suite_module
from .values import ContractError, canonical_bytes, require, sha256_bytes

CLI_PATH = "fixtures/performance/cli.toml"
# Shared by measurements and diagnostic child runs, including their untimed setup and
# seed writers. The frozen product must not install or check for another release.
ENVIRONMENT = {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "TZ": "UTC", "AF_SELF_OFFLINE": "1"}
_PLACEHOLDER = re.compile(r"\{([a-z_]+)\}")
CATALOG = ".af/task-catalog.toml"
PACKAGES = ".af/task-packages"


def load_bytes(data, where=CLI_PATH):
    doc = tomlfile.loads(data, where)
    require(doc.get("schema") == contracts.CLI_SCHEMA,
            where + ".schema: expected %s, found %r" % (contracts.CLI_SCHEMA, doc.get("schema")))
    return contracts.validate_cli(doc, where)


def load(path):
    with open(path, "rb") as stream:
        return load_bytes(stream.read(tomlfile.MAX_TOML_BYTES + 1), path)


def path_beside(suite_path):
    return os.path.join(os.path.dirname(os.path.abspath(suite_path)), "cli.toml")


def process_instances(suite_doc):
    """Every instance of a `process` case in any tier, by instance ID."""
    found = {}
    for tier in suite_doc["tiers"]:
        for item in suite_module.expand(suite_doc, tier):
            if suite_doc["cases"][item["case_id"]]["kind"] == "process":
                found[item["instance_id"]] = item
    return found


def _key(case_id, parameters):
    return case_id, canonical_bytes(parameters)


def check_registry(doc, suite_doc, where=CLI_PATH):
    """Every adapter declares exactly one command (runnable or pending) for every process
    instance of the registry, and nothing else; the state fixture is the registry's."""
    instances = process_instances(suite_doc)
    expected = dict((_key(item["case_id"], item["parameters"]), ident)
                    for ident, item in instances.items())
    for adapter_id, adapter in sorted(doc["adapters"].items()):
        declared = set()
        for command in adapter["commands"]:
            key = _key(command["case_id"], command["parameters"])
            require(key in expected, "%s.adapters.%s: %s %s is not a process instance of the "
                    "registry" % (where, adapter_id, command["case_id"],
                                  canonical_bytes(command["parameters"]).decode()))
            declared.add(key)
            for rule in command.get("rules", ()):
                # A seed's numbered events are its declared count, never the output's
                # (ADR-0171).
                if rule["kind"] == "json_stream_sequence" and "seed" in command:
                    events = doc["seeds"][command["seed"]]["events"]
                    require(rule["last"] - rule["first"] + 1 == events,
                            "%s.adapters.%s: %s numbers %d..%d, not the seed %s's %d events"
                            % (where, adapter_id, command["case_id"], rule["first"],
                               rule["last"], command["seed"], events))
        missing = sorted(expected[key] for key in set(expected) - declared)
        require(not missing, "%s.adapters.%s: no command for %s"
                % (where, adapter_id, ", ".join(missing)))
    fixtures = suite_doc["fixtures"]
    for seed_id, seed in sorted(doc["seeds"].items()):
        for value in seed["parameters"].values():
            fixture = fixtures.get(value)
            require(fixture is not None and fixture["kind"] == "cli_state"
                    and fixture.get("events") == seed["events"],
                    "%s.seeds.%s: %s is not a registry cli_state fixture of %d events"
                    % (where, seed_id, value, seed["events"]))
        require(seed["case_id"] not in suite_doc["cases"],
                "%s.seeds.%s: a seed writer is not a timed case" % (where, seed_id))
    return doc


# The task fixture's catalog -------------------------------------------------------------------

def package_digest(files):
    """The reviewer-package@1 digest of a package's files, by the independent reference
    encoding (perflab.reference), never by running the product."""
    listed = dict((path, reference.content_id(data)) for path, data in sorted(files.items()))
    return reference.content_id(reference.canonical({"kind": "reviewer-package@1",
                                                     "files": listed}))


def _package_files(root):
    files = {}
    for current, dirs, names in os.walk(root):
        dirs.sort()
        for name in sorted(names):
            path = os.path.join(current, name)
            require(stat.S_ISREG(os.lstat(path).st_mode), "%s is not a regular file" % path)
            with open(path, "rb") as stream:
                files[os.path.relpath(path, root).replace(os.sep, "/")] = stream.read()
    return files


def catalog_problems(repo):
    """How the fixture repository's committed catalog pins differ from the reference digests of
    the packages beside it; empty when every pin is exact and every package is pinned."""
    with open(os.path.join(repo, CATALOG), "rb") as stream:
        catalog = tomlfile.loads(stream.read(), CATALOG)
    problems = []
    pinned = catalog.get("packages", {})
    present = set()
    for owner in sorted(os.listdir(os.path.join(repo, PACKAGES))):
        for name in sorted(os.listdir(os.path.join(repo, PACKAGES, owner))):
            present.add("%s/%s" % (owner, name))
    for name in sorted(present | set(pinned)):
        entry = pinned.get(name)
        if entry is None:
            problems.append("%s is not pinned" % name)
            continue
        root = os.path.join(repo, entry["path"])
        if not os.path.isdir(root):
            problems.append("%s pins a missing path %s" % (name, entry["path"]))
            continue
        actual = package_digest(_package_files(root))
        if entry["digest"] != actual:
            problems.append("%s pins %s; its files digest to %s" % (name, entry["digest"],
                                                                    actual))
    return problems


LOCKFILE = ".af/af.lock"
PIPELINES = ".af/pipelines"


def pipeline_problems(repo):
    """How the Campaign fixture repository's committed pipeline pins differ from the reference
    content digests of the pipelines beside them; empty when every pipeline is pinned exactly."""
    with open(os.path.join(repo, LOCKFILE), "rb") as stream:
        lock = tomlfile.loads(stream.read(), LOCKFILE)
    pinned = lock.get("pipelines", {})
    present = set(name[:-len(".toml")] for name in os.listdir(os.path.join(repo, PIPELINES))
                  if name.endswith(".toml"))
    problems = []
    for name in sorted(present | set(pinned)):
        entry = pinned.get(name)
        if entry is None:
            problems.append("pipeline %s is not pinned" % name)
            continue
        path = os.path.join(repo, PIPELINES, name + ".toml")
        if not os.path.isfile(path):
            problems.append("pipeline %s pins a missing file" % name)
            continue
        with open(path, "rb") as stream:
            actual = reference.content_id(stream.read())
        if entry.get("digest") != actual:
            problems.append("pipeline %s pins %s; its file digests to %s"
                            % (name, entry.get("digest"), actual))
    return problems


def fixture_digest(directory):
    """One digest over a fixture directory's relative paths and bytes."""
    entries = []
    for current, dirs, names in os.walk(directory):
        dirs.sort()
        for name in sorted(names):
            path = os.path.join(current, name)
            with open(path, "rb") as stream:
                entries.append({"path": os.path.relpath(path, directory).replace(os.sep, "/"),
                                "sha256": sha256_bytes(stream.read())})
    return reference.content_id(canonical_bytes(sorted(entries, key=lambda e: e["path"])))


# Versions and adapters ------------------------------------------------------------------------

def package_version(lock, where="Cargo.lock"):
    """The `af` package's version that a source's Cargo.lock records."""
    doc = tomlfile.loads(lock, where)
    packages = [entry for entry in doc.get("package", []) if entry.get("name") == "af"]
    require(len(packages) == 1 and isinstance(packages[0].get("version"), str),
            where + ": records no single af package version")
    version = packages[0]["version"]
    require(gitrefs.parse_version(version) is not None,
            where + ": af %s is not a semantic version" % version)
    return version


def side_versions(git, plan):
    """Each measured side's own package version, from the Cargo.lock its source commit holds,
    whose bytes must be exactly the ones the plan bound by digest."""
    versions = {}
    for side, key in ((contracts.SIDES[0], "baseline_sha"), (contracts.SIDES[1], "candidate_sha")):
        sha = plan["resolved"][key]
        lock = git.read_file(sha, "Cargo.lock")
        require(lock is not None, "the %s source %s has no Cargo.lock" % (side, sha))
        expected = plan["build_inputs"][side]["cargo_lock_sha256"]
        require(sha256_bytes(lock) == expected, "the %s source's Cargo.lock is not the one the "
                "plan bound (%s)" % (side, expected))
        versions[side] = package_version(lock, "%s:Cargo.lock" % sha[:12])
    return versions


# The check receipt of ADR-0255. A side whose source holds this schema writes
# `af/TaskCheckReceipt@2` and refuses `@1` by name; a source before it writes `@1`.
RECEIPT_V2_SCHEMA = "schemas/task-check-receipt-v2.json"
RECEIPTS = ("receipt-v1", "receipt-v2")


def side_receipts(git, plan):
    """Each measured side's check-receipt generation, from the source commit the plan resolved
    for it, never from the binary's output: `receipt-v2` when that source holds the ADR-0255
    receipt schema, `receipt-v1` before it. The task fixture's setup declares the receipt its
    side's AF writes, so a pair across ADR-0255 measures the same Task on both sides."""
    receipts = {}
    for side, key in ((contracts.SIDES[0], "baseline_sha"), (contracts.SIDES[1], "candidate_sha")):
        sha = plan["resolved"][key]
        receipts[side] = RECEIPTS[git.read_file(sha, RECEIPT_V2_SCHEMA) is not None]
    return receipts


def setup_argument(entry, receipts, side, seed_argument):
    """The fifth argument of `cli/setup.sh` for `entry`'s setup: the side's receipt generation
    for a task-repository setup, which reads no seed; otherwise the seed's path or `-`."""
    if entry["setup"] in ("task_repository", "task_started"):
        return receipts.get(side, RECEIPTS[1])
    return seed_argument


def _version_key(text):
    return gitrefs.version_key(gitrefs.parse_version(text))


def choose_adapter(doc, version):
    """(adapter ID, adapter) of the first adapter by priority that covers `version`, or
    (None, reason)."""
    for adapter_id, adapter in sorted(doc["adapters"].items(),
                                      key=lambda item: item[1]["priority"]):
        if _version_key(version) >= _version_key(adapter["min_version"]):
            return adapter_id, adapter
    return None, "no declared CLI adapter covers AF %s" % version


def command(adapter, case_id, parameters):
    key = _key(case_id, parameters)
    for entry in adapter["commands"]:
        if _key(entry["case_id"], entry["parameters"]) == key:
            return entry
    return None


def expand(text, values):
    """Substitute the declared placeholders; anything else in braces is refused."""
    def substitute(match):
        name = match.group(1)
        require(name in values, "placeholder {%s} has no value here" % name)
        return values[name]
    return _PLACEHOLDER.sub(substitute, text)


def patterns(entry):
    """The byte patterns the supervisor must count, in rule order."""
    return [rule["pattern"] for rule in entry["rules"] if rule["kind"] == "stdout_count"]


JSON_STREAM_RULES = ("json_stream_equals", "json_stream_sequence", "json_stream_count")


def selectors(entry):
    """The JSON selectors the supervisor must observe over the whole stdout, one per
    `json_stream_*` rule, in rule order (ADR-0172)."""
    found = []
    for rule in entry["rules"]:
        if rule["kind"] == "json_stream_sequence":
            found.append({"path": rule["path"], "kind": "integer", "equals": []})
        elif rule["kind"] in JSON_STREAM_RULES:
            found.append({"path": rule["path"], "kind": "string", "equals": [rule["value"]]})
    return found


def _stream_problem(rule, document, selected):
    """Why the supervisor's observation of the whole stdout as one JSON document does not hold
    `rule`, or None (ADR-0172). Only the declared values and numbers decide; nothing is learned
    from the output."""
    if document["status"] != "complete":
        return "stdout is not one complete JSON document (%s at byte %s: %s)" % (
            document["status"], document["offset"], document["detail"])
    path = rule["path"]
    if selected["duplicates"]:
        return "%s is named %d more times than once in one object" % (
            path, selected["duplicates"])
    if selected["invalid"]:
        return "value %d of %s is not a %s" % (
            selected["first_invalid"] + 1, path,
            "plain nonnegative integer" if rule["kind"] == "json_stream_sequence"
            else "string")
    if rule["kind"] == "json_stream_equals":
        if selected["count"] != 1 or selected["matches"] != [1]:
            return "%s reaches %d values, not exactly one %s" % (
                path, selected["count"], json.dumps(rule["value"]))
        return None
    if rule["kind"] == "json_stream_count":
        if selected["matches"] != [rule["count"]]:
            return "%s is %s %d times, not %d" % (path, json.dumps(rule["value"]),
                                                   selected["matches"][0], rule["count"])
        return None
    wanted = rule["last"] - rule["first"] + 1
    if selected["count"] != selected["holders"]:
        return "%d values hold %s's parent, but %d hold its member" % (
            selected["holders"], path, selected["count"])
    broke = selected["first_break"]
    if broke is not None:
        return "value %d of %s is %s after %s; %d values break the sequence" % (
            broke["index"] + 1, path, _number(broke["value"]), _number(broke["after"]),
            selected["breaks"])
    if selected["count"] != wanted or selected["first"] != rule["first"] \
            or selected["last"] != rule["last"]:
        return "%s holds %d values numbered %s..%s, not %d numbered %d..%d" % (
            path, selected["count"], _number(selected["first"]), _number(selected["last"]),
            wanted, rule["first"], rule["last"])
    return None


def _number(value):
    return "no number" if value is None else str(value)


def seed_request(doc, seed_id, scratch):
    """The probe request of a seed's writer when its setup runs in `scratch`."""
    seed = doc["seeds"][seed_id]
    parameters = seed["parameters"]
    request = {"schema": contracts.PROBE_REQUEST_SCHEMA, "case_id": seed["case_id"],
               "instance_id": suite_module.instance_id(seed["case_id"],
                                                       sorted(parameters.items())),
               "contract_id": seed["contract_id"], "parameters": parameters,
               "repetitions": 1, "scratch": scratch + "/" + seed["probe_scratch"],
               "seed": None}
    return contracts.validate_probe_request(request)


# The oracle -----------------------------------------------------------------------------------

class _Missing(object):
    pass


MISSING = _Missing()


def _select(document, path):
    """Every value the dotted path reaches; `*` takes each element of a list."""
    current = [document]
    for segment in path.split("."):
        following = []
        for value in current:
            if segment == "*":
                if isinstance(value, list):
                    following.extend(value)
            elif isinstance(value, dict):
                if segment in value:
                    following.append(value[segment])
            elif isinstance(value, list) and segment.isdigit():
                index = int(segment)
                if index < len(value):
                    following.append(value[index])
        current = following
    return current


def _one(document, path):
    found = _select(document, path)
    return found[0] if len(found) == 1 else MISSING


def _same(left, right):
    return type(left) is type(right) and left == right


def _unique_pairs(pairs):
    seen = {}
    for key, value in pairs:
        if key in seen:
            raise ValueError("duplicate key %r" % key)
        seen[key] = value
    return seen


def _numbers(rule):
    return range(rule["first"], rule["last"] + 1)


def _exactly_once(found, rule, what):
    """Why the item numbers `found` are not each of first..last exactly once, or None."""
    seen = {}
    for number in found:
        seen[number] = seen.get(number, 0) + 1
    repeated = sorted(number for number, count in seen.items() if count > 1)
    missing = [number for number in _numbers(rule) if number not in seen]
    if repeated:
        return "%s %d appears %d times" % (what, repeated[0], seen[repeated[0]])
    if missing:
        return "no %s %d (%d of %d..%d missing)" % (what, missing[0], len(missing),
                                                    rule["first"], rule["last"])
    return None


def _distinct(values, name):
    """Why the values of `name` are not pairwise distinct strings, or None."""
    if any(not isinstance(value, str) for value in values):
        return "%s is not a string in every item" % name
    if len(set(values)) != len(values):
        return "two items share one %s" % name
    return None


def _enumerated_lines(rule, text, truncated):
    """Why stdout's lines are not exactly the declared items, or None."""
    if truncated or not text.endswith("\n"):
        return "stdout was not retained whole, so its items cannot all be read"
    pattern = re.compile(rule["pattern"])
    found, groups = [], dict((name, []) for name in rule["distinct"])
    for index, line in enumerate(text.split("\n")[:-1], 1):
        match = pattern.fullmatch(line)
        if match is None:
            return "line %d does not match the declared item: %r" % (index, line[:256])
        number = match.group("n")
        if not number.isdigit() or int(number) not in _numbers(rule):
            return "line %d names item %s, outside %d..%d" % (index, number, rule["first"],
                                                              rule["last"])
        found.append(int(number))
        for name in rule["distinct"]:
            groups[name].append(match.group(name))
    wrong = _exactly_once(found, rule, "item")
    for name in rule["distinct"]:
        wrong = wrong or _distinct(groups[name], name)
    return wrong


def _matches(item, rule, number):
    for key, template in rule["where"].items():
        if not _same(_one(item, key), template.replace("{n}", str(number))):
            return False
    for key in rule["numbered"]:
        if not _same(_one(item, key), number):
            return False
    return True


def _enumerated_items(rule, items):
    """Why `items` are not exactly the declared items, each once, or None."""
    if not isinstance(items, list):
        return "is not a list"
    found = []
    for index, item in enumerate(items):
        numbers = [number for number in _numbers(rule)
                   if isinstance(item, dict) and _matches(item, rule, number)]
        if len(numbers) != 1:
            return "item %d is %s declared item" % (
                index, "no" if not numbers else "more than one")
        found.append(numbers[0])
    wrong = _exactly_once(found, rule, "declared item")
    for key in rule["distinct"]:
        wrong = wrong or _distinct([_one(item, key) for item in items], key)
    return wrong


def _tally(items, field, counts):
    """Why the object `counts` is not the tally of `field` over `items`, or None."""
    if not isinstance(items, list) or not isinstance(counts, dict) or not counts:
        return "the list or the counts are absent"
    if any(type(value) is not int or value < 0 for value in counts.values()):
        return "a count is not a nonnegative integer"
    tally = dict((key, 0) for key in counts)
    for item in items:
        value = _one(item, field) if isinstance(item, dict) else MISSING
        if not isinstance(value, str) or value not in tally:
            return "an item's %s is %s, which the counts do not name" % (
                field, "absent" if value is MISSING else json.dumps(value))
        tally[value] += 1
    differing = sorted(key for key in counts if counts[key] != tally[key])
    if differing:
        return "%s counts %d, the items %d" % (differing[0], counts[differing[0]],
                                               tally[differing[0]])
    return None


def judge(entry, values, returncode, stdout, truncated, counts, observed_json):
    """(passed, detail, observed digest) of one CLI execution against its command's rules.

    `stdout` is the retained output, `truncated` how many bytes past it the child wrote,
    `counts` the supervisor's counts of `patterns(entry)` over all of it and `observed_json`
    its observation of `selectors(entry)` over all of it as one JSON document. Empty output
    fails, as does any rule that does not hold; the first failing rule is the detail."""
    observed = sha256_bytes(stdout)
    if not stdout and not truncated:
        return False, "the command wrote nothing to stdout; empty output never passes", observed
    try:
        text = stdout.decode("utf-8")
    except UnicodeDecodeError:
        text = None
    document = MISSING
    count_index = 0
    selector_index = 0
    for index, rule in enumerate(entry["rules"]):
        kind = rule["kind"]
        where = "rule %d (%s)" % (index + 1, kind)
        if kind == "exit_code":
            if returncode != rule["value"]:
                return False, "%s: the command exited %s, not %d" % (
                    where, returncode, rule["value"]), observed
            continue
        if kind == "stdout_count":
            found = counts[count_index]
            count_index += 1
            if found != rule["value"]:
                return False, "%s: %r occurs %d times in stdout, not %d" % (
                    where, rule["pattern"], found, rule["value"]), observed
            continue
        if kind in JSON_STREAM_RULES:
            wrong = _stream_problem(rule, observed_json["document"],
                                    observed_json["selectors"][selector_index])
            selector_index += 1
            if wrong:
                return False, "%s: %s" % (where, wrong), observed
            continue
        if text is None:
            return False, "%s: stdout is not UTF-8" % where, observed
        if kind == "stdout_contains":
            if rule["value"] not in text:
                return False, "%s: stdout lacks %r" % (where, rule["value"]), observed
            continue
        if kind == "stdout_enumerates":
            wrong = _enumerated_lines(rule, text, truncated)
            if wrong:
                return False, "%s: %s" % (where, wrong), observed
            continue
        if kind in ("stdout_lines", "stdout_line"):
            # The last piece is the partial line after the final newline, or empty.
            complete = text.split("\n")[:-1]
            if kind == "stdout_lines":
                if truncated or not text.endswith("\n") or len(complete) != rule["value"]:
                    return False, "%s: stdout holds %s lines, not %d" % (
                        where, "more" if truncated else len(complete), rule["value"]), observed
            else:
                expected = expand(rule["equals"], values)
                if rule["index"] >= len(complete) or complete[rule["index"]] != expected:
                    shown = complete[rule["index"]] if rule["index"] < len(complete) else None
                    return False, "%s: line %d is %r, not %r" % (
                        where, rule["index"] + 1, shown, expected), observed
            continue
        # JSON rules read the whole document.
        if document is MISSING:
            if truncated:
                return False, "%s: stdout was truncated after %d bytes, so its JSON document " \
                    "cannot be read" % (where, len(stdout)), observed
            try:
                document = json.loads(text, object_pairs_hook=_unique_pairs)
            except ValueError as error:
                return False, "%s: stdout is not one JSON document: %s" % (where, error), \
                    observed
        if kind == "json_equals":
            value = _one(document, rule["path"])
            if value is MISSING or not _same(value, rule["value"]):
                return False, "%s: %s is %s, not %s" % (
                    where, rule["path"], "absent" if value is MISSING else json.dumps(value),
                    json.dumps(rule["value"])), observed
        elif kind == "json_matches":
            value = _one(document, rule["path"])
            if not isinstance(value, str) or re.search(rule["pattern"], value) is None:
                return False, "%s: %s does not match %s" % (where, rule["path"],
                                                            rule["pattern"]), observed
        elif kind == "json_count":
            value = _one(document, rule["path"])
            if not isinstance(value, list) or len(value) != rule["value"]:
                return False, "%s: %s holds %s items, not %d" % (
                    where, rule["path"], len(value) if isinstance(value, list) else "no",
                    rule["value"]), observed
        elif kind == "json_contiguous":
            found = _select(document, rule["path"])
            wanted = list(range(rule["start"], rule["start"] + rule["count"]))
            if [item for item in found if type(item) is int] != wanted or len(found) != \
                    len(wanted):
                return False, "%s: %s is not exactly %d..%d in order" % (
                    where, rule["path"], wanted[0], wanted[-1]), observed
        elif kind == "json_enumerates":
            wrong = _enumerated_items(rule, _one(document, rule["path"]))
            if wrong:
                return False, "%s: %s %s" % (where, rule["path"], wrong), observed
        elif kind == "json_tally":
            wrong = _tally(_one(document, rule["path"]), rule["field"],
                           _one(document, rule["counts"]))
            if wrong:
                return False, "%s: %s is not the tally of %s.*.%s: %s" % (
                    where, rule["counts"], rule["path"], rule["field"], wrong), observed
        elif kind == "json_any":
            candidates = []
            for value in _select(document, rule["path"]):
                candidates.extend(value if isinstance(value, list) else [value])
            if not any(isinstance(candidate, dict)
                       and all(_same(_one(candidate, key), expected)
                               for key, expected in rule["where"].items())
                       for candidate in candidates):
                return False, "%s: no %s has %s" % (where, rule["path"], json.dumps(
                    rule["where"], sort_keys=True)), observed
        else:
            raise ContractError("unknown CLI rule " + kind)
    return True, "all %d rules hold" % len(entry["rules"]), observed
