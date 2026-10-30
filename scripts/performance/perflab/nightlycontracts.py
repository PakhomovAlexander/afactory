"""Closed contracts of the owner's nightly build (ADR-0265), the daily report composed
from the night's record and the executor journal (ADR-0269) and the publication
(ADR-0270): the configuration, the plan `build` prints, the record each night leaves in
its directory, the report record and the receipt `publish` writes beside it.

A nightly is a separate train from the releases: its tag is `nightly-YYYY.MM.DD` and its
version `<core>-nightly.<YYYYMMDD>`, both derived here from the night's date and the
workspace version on the chosen commit, so a reader recomputes them instead of trusting them.
"""
import datetime
from fractions import Fraction
import re

from .spec import (Arr, Bool, COUNT, Const, DIGEST, Field, GIT_SHA, ID, Int, Map, Nullable, Obj,
                   RATIONAL, Str, UNIT)
from .values import ContractError, format_rational, parse_rational, require

CONFIG_SCHEMA = "af.nightly-config/1"
PLAN_SCHEMA = "af.nightly-plan/1"
BUILD_SCHEMA = "af.nightly-build/1"
PUBLISH_SCHEMA = "af.nightly-publish/1"

# The Linux targets a nightly ships, in the order they are built and recorded. The VM cannot
# build macOS, so a nightly has no aarch64-apple-darwin archive.
TARGETS = ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl")
SUMS = "dist/SHA256SUMS"
SIGNATURE = "dist/SHA256SUMS.minisig"
DATE_PATTERN = r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$"
TAG_PATTERN = r"^nightly-[0-9]{4}\.[0-9]{2}\.[0-9]{2}$"
VERSION_PATTERN = r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-nightly\.[0-9]{8}$"
# The files a published night's release carries after its two archives, in upload order: the
# signed sums and the build record `build` wrote, then the report N3 composed. Only the report
# (and the release notes) is ever replaced.
SHIPPED_FILES = ("SHA256SUMS", "SHA256SUMS.minisig", "nightly.json")
REPORT_FILES = ("daily.json", "report.html")
KEEP_NIGHTS_DEFAULT = 14
TELEGRAM_CHAT_PATTERN = r"^(-?[1-9][0-9]{0,19}|@[A-Za-z][A-Za-z0-9_]{4,31})$"
# The workspace version a nightly is derived from: plain SemVer, no build metadata.
_WORKSPACE_VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
                                r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$")
# Jobs of the release workflow that run on a main commit beside its `check / …` jobs; a
# nightly's greenness never depends on them, so a configuration cannot require them.
RELEASE_JOBS = ("resolve the release", "sign and publish", "pin the release in .af/af.lock")
RELEASE_JOB_PREFIX = "build "
CHECK_PREFIX = "check /"
DAILY_SCHEMA = "af.nightly-daily/1"
# The daily report's sections, in the order the page shows them after its headline; each has
# one completeness entry (ADR-0269).
SECTIONS = ("build", "baseline", "performance", "telemetry", "trend", "since_last_night",
            "backlog", "machine")
LABELS = ("PASS", "FAIL", "INCOMPLETE", "UNAVAILABLE")
HISTORY_NIGHTS_DEFAULT, HISTORY_NIGHTS_MAX = 14, 60
HEADLINE_INSTANCES_MAX = 4
# A verdict case that did not move: its interval lies inside the tolerance, or not above it,
# and it passed. Every other case is a moved one the report lists by itself.
UNCHANGED = ("within_tolerance", "non_regression")
TELEMETRY_SHOWN = ("overhead", "read_latency")
# The most items the nightly GitHub reader returns from one list (its 20 pages of 100).
GITHUB_ITEMS_MAX = 2000
# The most open pull requests whose mergeable state the backlog reads, one request each; more
# open pull requests are a partial read the backlog records with its reason.
PULL_REQUESTS_MAX = 100


def three_significant(value):
    """An exact rational (its text or a Fraction) as a decimal of three significant digits,
    rounded half away from zero with exact arithmetic: 21/2 -> 10.5, 50 -> 50.0, 1/3 -> 0.333,
    1234 -> 1230, 0 -> 0."""
    number = Fraction(value)
    if number == 0:
        return "0"
    sign, number = ("-" if number < 0 else ""), abs(number)
    exponent = len(str(number.numerator)) - len(str(number.denominator))
    while Fraction(10) ** exponent > number:
        exponent -= 1
    while Fraction(10) ** (exponent + 1) <= number:
        exponent += 1
    scaled = number / Fraction(10) ** (exponent - 2)
    digits = int(scaled)
    if scaled - digits >= Fraction(1, 2):
        digits += 1
    if digits == 1000:
        digits, exponent = 100, exponent + 1
    text = str(digits)
    if exponent >= 2:
        return sign + text + "0" * (exponent - 2)
    decimals = 2 - exponent
    if decimals >= 3:
        return sign + "0." + "0" * (decimals - 3) + text
    return sign + text[:3 - decimals] + "." + text[3 - decimals:]


def number(value):
    """The daily report's number: the exact rational and its three-significant-digit text."""
    text = format_rational(Fraction(value))
    return {"exact": text, "shown": three_significant(text)}


def night_date(text, where="date"):
    """The calendar date `text` names as YYYY-MM-DD, or refused."""
    require(isinstance(text, str) and re.match(DATE_PATTERN, text) is not None,
            "%s: %r is not a YYYY-MM-DD date" % (where, text))
    try:
        return datetime.date(int(text[0:4]), int(text[5:7]), int(text[8:10]))
    except ValueError:
        raise ContractError("%s: %r is not a calendar date" % (where, text))


def nightly_tag(date):
    """`nightly-YYYY.MM.DD`: never `vX.Y.Z…`, so no release tool takes it for a release."""
    night_date(date)
    return "nightly-" + date.replace("-", ".")


def nightly_version(workspace_version, date):
    """`<core>-nightly.<YYYYMMDD>`: a pre-release workspace version keeps its core
    (0.14.0-rc.3 -> 0.14.0), a plain release bumps its minor and zeroes its patch
    (0.14.0 -> 0.15.0), so a nightly always sorts after the release main last shipped."""
    night_date(date)
    match = _WORKSPACE_VERSION.match(workspace_version) if isinstance(
        workspace_version, str) else None
    require(match is not None, "workspace version %r is not a SemVer version without build "
            "metadata" % (workspace_version,))
    major, minor, patch, pre = match.groups()
    if pre:
        core = "%s.%s.%s" % (major, minor, patch)
    else:
        core = "%s.%d.0" % (major, int(minor) + 1)
    return "%s-nightly.%s" % (core, date.replace("-", ""))


def asset_name(tag, target):
    """The archive of one target, named as the release names its own: `af-<tag>-<target>`."""
    require(target in TARGETS, "%s is not a nightly target" % target)
    return "af-%s-%s.tar.gz" % (tag, target)


def daily_node(c):
    """The shape of `af.nightly-daily/1`, one night's daily report record (ADR-0269)."""
    tag = Str(pattern=TAG_PATTERN, max_len=19)
    reason = Str(min_len=1, max_len=2048)
    text = Str(max_len=1024)
    shown = Obj(Field("exact", RATIONAL),
                Field("shown", Str(pattern=r"^-?[0-9]+(?:\.[0-9]+)?$", min_len=1, max_len=600)))
    fact = Obj(Field("value", Nullable(COUNT)), Field("reason", Nullable(reason)))
    held = Obj(Field("value", Nullable(Bool())), Field("reason", Nullable(reason)))
    build = Obj(
        Field("status", Str(enum=("built", "skipped"))), Field("head_sha", GIT_SHA),
        Field("head_subject", Nullable(text)), Field("version", Str(pattern=VERSION_PATTERN,
                                                                   max_len=96)),
        Field("previous_tag", Nullable(tag)), Field("previous_head_sha", Nullable(GIT_SHA)),
        Field("skip_reason", Nullable(reason)),
        Field("targets", Arr(Obj(Field("target", Str(enum=TARGETS)),
                                 Field("asset", Str(min_len=1, max_len=128)),
                                 Field("sha256", DIGEST), Field("bytes", Int(1, 2 ** 40)),
                                 Field("af_sha256", Nullable(DIGEST)),
                                 Field("af_reason", Nullable(reason))), 0, len(TARGETS))))
    baseline = Obj(Field("tag", Str(min_len=1, max_len=256)), Field("sha", GIT_SHA),
                   Field("published", Nullable(c.DATE)), Field("commits_since", Nullable(COUNT)),
                   Field("pull_requests_since", Nullable(COUNT)),
                   Field("reasons", Arr(reason, 0, 4)))
    instance = Obj(
        Field("instance_id", ID), Field("criticality", Str(enum=("critical", "advisory"))),
        Field("unit", UNIT), Field("baseline_median", Nullable(shown)),
        Field("candidate_median", Nullable(shown)), Field("relative_change", Nullable(shown)),
        Field("interval", Nullable(Obj(Field("lower", shown), Field("upper", shown)))),
        Field("status", Str(enum=c.CASE_STATUSES)),
        Field("classification", Nullable(Str(enum=c.READINESS_CLASSIFICATIONS))),
        Field("outcome", Str(enum=("pass", "fail", "incomplete"))), Field("moved", Bool()))
    performance = Obj(
        Field("request_id", ID), Field("run_id", ID), Field("plan_digest", DIGEST),
        Field("verdict_sha256", DIGEST),
        Field("result", Str(enum=("pass", "fail", "incomplete"))), Field("exit_code", Int(0, 4)),
        Field("scope", Str(enum=c.SCOPES)),
        Field("provenance", Str(enum=("measured", "synthetic", "mixed", "none"))),
        Field("headline", Str(min_len=1, max_len=2048)),
        Field("satisfies_required_check", Bool()),
        Field("reasons", Arr(Obj(Field("code", ID), Field("instance_id", Nullable(ID)),
                                 Field("detail", Str(max_len=4096))), 0, 64)),
        Field("instances", Arr(instance, 0, 4096)), Field("unchanged", COUNT),
        Field("coverage", Obj(Field("expected_records", COUNT), Field("matched_records", COUNT),
                              Field("missing_records", COUNT))),
        Field("sentinel", Obj(Field("shards", COUNT), Field("healthy", COUNT),
                              Field("flags", Arr(ID, 0, 64, unique=True)))),
        Field("measured_candidate_sha256", Nullable(DIGEST)),
        Field("shipped_sha256", Nullable(DIGEST)), Field("measured_is_shipped", Nullable(Bool())),
        Field("measured_reason", Nullable(reason)))
    telemetry = Obj(
        Field("request_id", ID), Field("result_id", ID),
        Field("figures", Arr(Obj(
            Field("experiment", Str(enum=TELEMETRY_SHOWN)), Field("subject", Str(min_len=1,
                                                                                 max_len=256)),
            Field("level", Str(enum=c.TELEMETRY_LEVEL_NAMES)),
            Field("metric", Str(enum=c.TELEMETRY_METRICS)), Field("unit", UNIT),
            Field("state", Str(enum=("compared", "incompatible", "missing"))),
            Field("median_change_percent", Nullable(shown))), 0, 1024)))
    trend = Obj(
        Field("nights", Int(1, HISTORY_NIGHTS_MAX)),
        Field("instances", Arr(Obj(
            Field("instance_id", ID), Field("unit", Nullable(UNIT)),
            Field("points", Arr(Obj(Field("tag", tag), Field("baseline_tag", Str(min_len=1,
                                                                                 max_len=256)),
                                    Field("candidate_median", Nullable(shown)),
                                    Field("release", Nullable(Str(min_len=1, max_len=256)))),
                                0, HISTORY_NIGHTS_MAX + 1))), 0, HEADLINE_INSTANCES_MAX)),
        Field("refused", Arr(Obj(Field("tag", tag), Field("reason", reason)),
                             0, HISTORY_NIGHTS_MAX + 1)))
    pull = Obj(Field("number", Int(1, 2 ** 31)), Field("title", text))
    since = Obj(
        Field("previous_tag", Nullable(tag)), Field("previous_head_sha", Nullable(GIT_SHA)),
        Field("merged_pull_requests", Nullable(Arr(pull, 0, GITHUB_ITEMS_MAX))),
        Field("verdict_changes", Nullable(Arr(Obj(Field("instance_id", ID),
                                                  Field("previous", Str(enum=c.CASE_STATUSES)),
                                                  Field("current", Str(enum=c.CASE_STATUSES))),
                                              0, 4096))),
        Field("reasons", Arr(reason, 0, 4)))
    backlog = Obj(
        Field("open_issues", COUNT),
        Field("labels", Arr(Obj(Field("name", Str(min_len=1, max_len=256)),
                                Field("count", COUNT)), 0, GITHUB_ITEMS_MAX)),
        Field("opened_24h", COUNT), Field("closed_24h", COUNT),
        Field("stale_issues", COUNT),
        Field("stale", Arr(Obj(Field("number", Int(1, 2 ** 31)), Field("title", text),
                               Field("updated", c.DATE)), 0, 20)),
        Field("open_pull_requests", COUNT), Field("draft_pull_requests", COUNT),
        Field("pull_requests", Arr(Obj(Field("number", Int(1, 2 ** 31)), Field("title", text),
                                       Field("draft", Bool()),
                                       Field("mergeable_state", Str(min_len=1, max_len=32))),
                                   0, PULL_REQUESTS_MAX)),
        Field("pull_requests_partial", Nullable(reason)),
        Field("head_checks", Obj(Field("total", COUNT), Field("success", COUNT),
                                 Field("failure", COUNT), Field("skipped", COUNT),
                                 Field("pending", COUNT), Field("other", COUNT))))
    machine = Obj(Field("free_bytes", fact), Field("lock_held", held),
                  Field("failed_units", fact))
    entry = Obj(Field("available", Bool()), Field("reason", reason))
    return Obj(
        Field("schema", Const(DAILY_SCHEMA)),
        Field("date", Str(pattern=DATE_PATTERN, max_len=10)), Field("tag", tag),
        Field("composed_unix_ms", c.UNIX_MS), Field("repository", c.REPOSITORY),
        Field("host", Obj(Field("machine_id", Nullable(ID)))),
        Field("headline", Obj(Field("label", Str(enum=LABELS)),
                              Field("text", Str(min_len=1, max_len=2048)))),
        Field("build", build), Field("baseline", baseline),
        Field("performance", Nullable(performance)), Field("telemetry", Nullable(telemetry)),
        Field("trend", Nullable(trend)), Field("since_last_night", Nullable(since)),
        Field("backlog", Nullable(backlog)), Field("machine", machine),
        Field("completeness", Obj(*[Field(name, entry) for name in SECTIONS])))


def _numbers(value, where):
    """Every `{exact, shown}` number below `value` shows its exact rational's three
    significant digits."""
    if isinstance(value, dict):
        if set(value) == {"exact", "shown"}:
            require(value["shown"] == three_significant(parse_rational(value["exact"], where)),
                    where + ".shown: the exact rational to three significant digits")
            return
        for key, item in value.items():
            _numbers(item, where + "." + key)
    elif isinstance(value, list):
        for index, item in enumerate(value):
            _numbers(item, "%s[%d]" % (where, index))


def validate_daily_semantics(doc, where):
    require(doc["tag"] == nightly_tag(doc["date"]),
            where + ".tag: a night's tag is nightly-YYYY.MM.DD of its date")
    _numbers(doc, where)
    for name in SECTIONS:
        require(doc["completeness"][name]["available"] is False
                or doc[name] is not None,
                where + ".completeness.%s: available only when the section is recorded" % name)
    for name, fact in doc["machine"].items():
        require((fact["value"] is None) == (fact["reason"] is not None),
                where + ".machine.%s: a value, or null with the reason" % name)
    performance = doc["performance"]
    label = "UNAVAILABLE" if performance is None else performance["result"].upper()
    require(doc["headline"]["label"] == label,
            where + ".headline.label: the verdict's result, or UNAVAILABLE without one")
    build = doc["build"]
    skipped = build["status"] == "skipped"
    require(skipped == (build["skip_reason"] is not None) and (not skipped or not build["targets"]),
            where + ".build: a skipped night has its reason and no archives")
    if skipped:
        require(all(doc[name] is None for name in ("performance", "telemetry", "trend",
                                                   "since_last_night")),
                where + ": a skipped night measures nothing, so it has no performance, "
                "telemetry, trend or since-last-night section")
    for entry in build["targets"]:
        require((entry["af_sha256"] is None) == (entry["af_reason"] is not None),
                where + ".build.targets: the shipped af's digest, or null with the reason")
    if performance is not None:
        ids = [item["instance_id"] for item in performance["instances"]]
        require(len(ids) == len(set(ids)), where + ".performance.instances: each instance once")
        for item in performance["instances"]:
            require(item["moved"] == (item["status"] not in UNCHANGED
                                      or item["outcome"] != "pass"),
                    where + ".performance.instances.%s.moved: every case that is not unchanged "
                    "and passing" % item["instance_id"])
        require(performance["unchanged"] == sum(1 for item in performance["instances"]
                                                if not item["moved"]),
                where + ".performance.unchanged: the count of instances that did not move")
        measured, shipped = performance["measured_candidate_sha256"], performance["shipped_sha256"]
        same = None if measured is None or shipped is None else measured == shipped
        require(performance["measured_is_shipped"] == same
                and (same is None) == (performance["measured_reason"] is not None),
                where + ".performance.measured_is_shipped: the measured candidate's digest "
                "compared with the shipped af's, or null with the reason")
    if doc["trend"] is not None:
        for item in doc["trend"]["instances"]:
            tags = [point["tag"] for point in item["points"]]
            require(tags == sorted(set(tags)) and len(tags) <= doc["trend"]["nights"] + 1
                    and all(tag <= doc["tag"] for tag in tags),
                    where + ".trend.%s: at most the configured earlier nights and this one, "
                    "each once and in night order" % item["instance_id"])
    backlog = doc["backlog"]
    if backlog is not None:
        listed, total = len(backlog["pull_requests"]), backlog["open_pull_requests"]
        require(listed == min(total, PULL_REQUESTS_MAX)
                and (backlog["pull_requests_partial"] is None) == (total <= PULL_REQUESTS_MAX),
                where + ".backlog.pull_requests: every open pull request's mergeable state, or "
                "the first %d and the partial read's reason" % PULL_REQUESTS_MAX)
    return doc
def release_assets(tag):
    """The names of a published night's release assets, in upload order."""
    return [asset_name(tag, target) for target in TARGETS] + list(SHIPPED_FILES + REPORT_FILES)


def release_url(repository, tag):
    """The release page GitHub reports as a release's `html_url`."""
    return "https://github.com/%s/releases/tag/%s" % (repository, tag)


def register(c):
    SHA, DIGEST = c.GIT_SHA, c.DIGEST
    path = c.ABSOLUTE_PATH
    check_name = Str(min_len=1, max_len=256)
    configuration = Obj(
        Field("schema", Const(CONFIG_SCHEMA)), Field("repository", c.REPOSITORY),
        Field("mirror", path), Field("builds_root", path),
        Field("build_image", c.images.REFERENCE), Field("qemu_aarch64", path),
        Field("sign_key", path), Field("public_key", path),
        Field("required_checks", Arr(check_name, 0, 64, unique=True)),
        Field("max_walk", Int(1, 200), required=False),
        Field("github_token_file", path, required=False),
        # What `report.py compose` reads besides the night's record (ADR-0269).
        Field("report", Obj(Field("executor_config", path),
                            Field("history_nights", Int(1, HISTORY_NIGHTS_MAX), required=False),
                            Field("headline_instances",
                                  Arr(ID, 1, HEADLINE_INSTANCES_MAX, unique=True))),
              required=False),
        # Publishing (ADR-0270); the build never reads this table.
        Field("publish", Obj(
            Field("github_token_file", path),
            Field("telegram_token_file", path, required=False),
            Field("telegram_chat_id", Str(pattern=TELEGRAM_CHAT_PATTERN, max_len=33),
                  required=False),
            Field("keep_nights", Int(3, 60), required=False)), required=False))
    rejected = Obj(Field("sha", SHA), Field("reason", Str(min_len=1, max_len=2048)))
    common = (
        Field("date", Str(pattern=DATE_PATTERN, max_len=10)),
        Field("tag", Str(pattern=TAG_PATTERN, max_len=19)),
        Field("version", Str(pattern=VERSION_PATTERN, max_len=96)),
        Field("head_sha", SHA), Field("head_committed_unix", Int(0, 2 ** 53)),
        Field("rejected", Arr(rejected, 0, 200)),
        Field("previous_head_sha", Nullable(SHA)),
        Field("previous_tag", Nullable(Str(pattern=TAG_PATTERN, max_len=19))),
        Field("baseline_tag", Str(min_len=1, max_len=256)), Field("baseline_sha", SHA),
        Field("cargo_version_on_main", Str(min_len=1, max_len=80)),
        Field("skip_reason", Nullable(Str(min_len=1, max_len=1024))))
    plan = Obj(Field("schema", Const(PLAN_SCHEMA)),
               Field("status", Str(enum=("build", "skipped"))), *common)
    target = Obj(
        Field("target", Str(enum=TARGETS)),
        Field("asset", Str(pattern=r"^dist/af-nightly-[0-9.]{10}-[a-z0-9_-]+\.tar\.gz$",
                           max_len=128)),
        Field("sha256", DIGEST), Field("bytes", Int(1, 2 ** 40)),
        Field("smoke", Const("passed")),
        Field("version_json", Str(pattern=r"^version-[a-z0-9_-]+\.json$", max_len=96)))
    phase = Obj(Field("started_unix_ms", c.UNIX_MS), Field("ended_unix_ms", c.UNIX_MS))
    record = Obj(
        Field("schema", Const(BUILD_SCHEMA)),
        Field("status", Str(enum=("built", "skipped"))), *common,
        Field("build_image", c.images.REFERENCE),
        Field("targets", Arr(target, 0, len(TARGETS))),
        Field("sha256sums", Nullable(Obj(Field("path", Const(SUMS)),
                                         Field("signature_path", Const(SIGNATURE))))),
        Field("timings", Map(r"^[a-z0-9][a-z0-9_.-]{0,63}$", phase, 1, 32)))
    daily = daily_node(c)
    tag_name = Str(pattern=TAG_PATTERN, max_len=19)
    uploaded = Obj(Field("name", Str(pattern=r"^[A-Za-z0-9._-]+$", max_len=128)),
                   Field("size", Int(1, 2 ** 40)), Field("digest", DIGEST))
    receipt = Obj(
        Field("schema", Const(PUBLISH_SCHEMA)),
        Field("status", Str(enum=("published", "skipped"))),
        Field("date", Str(pattern=DATE_PATTERN, max_len=10)), Field("tag", tag_name),
        Field("head_sha", SHA), Field("repository", c.REPOSITORY),
        Field("release", Nullable(Obj(
            Field("id", Int(1, 2 ** 53)),
            Field("url", Str(pattern=r"^https://github\.com/", max_len=512))))),
        Field("assets", Arr(uploaded, 0, len(TARGETS) + len(SHIPPED_FILES + REPORT_FILES))),
        Field("telegram", Nullable(Obj(Field("message_id", Int(1, 2 ** 53))))),
        Field("telegram_reason", Nullable(Str(min_len=1, max_len=1024))),
        Field("pruned", Arr(tag_name, 0, 1024, unique=True)),
        Field("prune_failures", Arr(Obj(Field("tag", tag_name),
                                        Field("reason", Str(min_len=1, max_len=1024))),
                                    0, 1024)),
        Field("foreign", Arr(tag_name, 0, 1024, unique=True)),
        Field("amendments", Int(0, 10000)),
        Field("published_unix_ms", c.UNIX_MS))

    def validate_config(doc, where="nightly.toml"):
        configuration.validate(doc, where)
        for name in doc["required_checks"]:
            require(name not in RELEASE_JOBS and not name.startswith(RELEASE_JOB_PREFIX),
                    "%s.required_checks: %r is a job of the release workflow, which a "
                    "nightly never waits for" % (where, name))
        publish = doc.get("publish")
        if publish is not None:
            require(("telegram_token_file" in publish) == ("telegram_chat_id" in publish),
                    where + ".publish: telegram_token_file and telegram_chat_id are set together "
                    "or not at all")
            require(publish["github_token_file"] != doc.get("github_token_file"),
                    where + ".publish.github_token_file: the write token is not the read-only "
                    "github_token_file")
        root = doc["builds_root"].rstrip("/") + "/"
        mirror = doc["mirror"].rstrip("/") + "/"
        require(not root.startswith(mirror) and not mirror.startswith(root),
                where + ": builds_root and mirror lie apart")
        return doc

    def validate_common(doc, where):
        night_date(doc["date"], where + ".date")
        require(doc["tag"] == nightly_tag(doc["date"]),
                where + ".tag: a night's tag is nightly-YYYY.MM.DD of its date")
        require(doc["version"] == nightly_version(doc["cargo_version_on_main"], doc["date"]),
                where + ".version: not the nightly version of cargo_version_on_main and date")
        shas = [entry["sha"] for entry in doc["rejected"]]
        require(len(set(shas)) == len(shas) and doc["head_sha"] not in shas,
                where + ".rejected: each walked commit once, never the chosen one")
        require((doc["previous_head_sha"] is None) == (doc["previous_tag"] is None),
                where + ": a previous night has both a head and a tag, or neither")
        require(doc["previous_tag"] is None or doc["previous_tag"] < doc["tag"],
                where + ".previous_tag: the previous night precedes this one")
        skipped = doc["status"] == "skipped"
        require(skipped == (doc["skip_reason"] is not None),
                where + ".skip_reason: present exactly on a skipped night")
        require(not skipped or doc["previous_head_sha"] == doc["head_sha"],
                where + ": a night is skipped only when its commit is the previous night's")
        require(skipped or doc["previous_head_sha"] != doc["head_sha"],
                where + ": a night on the previous night's commit is skipped, never built")

    def validate_plan(doc, where="nightly plan"):
        plan.validate(doc, where)
        validate_common(doc, where)
        return doc

    def validate_record(doc, where="nightly.json"):
        record.validate(doc, where)
        validate_common(doc, where)
        for name, span in doc["timings"].items():
            require(span["started_unix_ms"] <= span["ended_unix_ms"],
                    "%s.timings.%s: a phase ends after it starts" % (where, name))
        if doc["status"] == "skipped":
            require(not doc["targets"] and doc["sha256sums"] is None,
                    where + ": a skipped night has no archives and no SHA256SUMS")
            return doc
        require([entry["target"] for entry in doc["targets"]] == list(TARGETS),
                where + ".targets: every nightly target, in order: " + ", ".join(TARGETS))
        require(doc["sha256sums"] is not None, where + ".sha256sums: a built night signs them")
        for entry in doc["targets"]:
            require(entry["asset"] == "dist/" + asset_name(doc["tag"], entry["target"]),
                    "%s.targets.%s.asset: expected dist/%s" % (
                        where, entry["target"], asset_name(doc["tag"], entry["target"])))
            require(entry["version_json"] == "version-%s.json" % entry["target"],
                    "%s.targets.%s.version_json: expected version-%s.json" % (
                        where, entry["target"], entry["target"]))
        return doc

    def validate_receipt(doc, where="publish.json"):
        receipt.validate(doc, where)
        night_date(doc["date"], where + ".date")
        require(doc["tag"] == nightly_tag(doc["date"]),
                where + ".tag: a night's tag is nightly-YYYY.MM.DD of its date")
        require((doc["telegram"] is None) != (doc["telegram_reason"] is None),
                where + ": a Telegram message id, or the reason there is none")
        require(doc["tag"] not in doc["foreign"],
                where + ".foreign: a night never refuses its own release")
        failed = [entry["tag"] for entry in doc["prune_failures"]]
        require(len(set(failed)) == len(failed) and not set(failed) & set(doc["pruned"]),
                where + ".prune_failures: each tag once, never one pruned")
        if doc["status"] == "skipped":
            require(doc["release"] is None and not doc["assets"] and not doc["pruned"]
                    and not doc["prune_failures"] and not doc["foreign"],
                    where + ": a skipped night publishes, uploads and prunes nothing")
            return doc
        require(doc["release"] is not None, where + ".release: a published night has one")
        require(doc["release"]["url"] == release_url(doc["repository"], doc["tag"]),
                where + ".release.url: expected " + release_url(doc["repository"], doc["tag"]))
        require([entry["name"] for entry in doc["assets"]] == release_assets(doc["tag"]),
                where + ".assets: every asset of the night, in order: "
                + ", ".join(release_assets(doc["tag"])))
        return doc

    c.validate_nightly_config = validate_config
    c.validate_nightly_plan = validate_plan
    c.validate_nightly_build = validate_record

    def validate_daily(doc, where="daily.json"):
        daily.validate(doc, where)
        return validate_daily_semantics(doc, where)

    c.validate_nightly_daily = validate_daily
    c.validate_nightly_publish = validate_receipt
    c.CONTRACTS["nightly-config"] = (
        CONFIG_SCHEMA, configuration, validate_config,
        "The owner's nightly build configuration as parsed TOML (ADR-0265): the repository, "
        "the bare mirror and builds root, the pinned musl build image, the aarch64 user-mode "
        "runner, the nightly signing key pair, the check runs a green commit must carry and "
        "how far back the walk may go, and, for publishing (ADR-0270), the write token, the "
        "optional Telegram bot and chat and how many nightly releases stay.")
    c.CONTRACTS["nightly-plan"] = (
        PLAN_SCHEMA, plan, validate_plan,
        "What one night would build (ADR-0265): the newest green first-parent commit of main "
        "and every red commit walked over, the previous night, the stable baseline, the "
        "nightly tag and version, and whether the night is skipped because main has not moved.")
    c.CONTRACTS["nightly-build"] = (
        BUILD_SCHEMA, record, validate_record,
        "One night's nightly build record, builds_root/<date>/nightly.json (ADR-0265): the "
        "plan it built, the build image, each Linux target's archive with its digest, size, "
        "smoke and version report, the signed SHA256SUMS and every phase's timing, or why "
        "the night was skipped.")
    c.CONTRACTS["nightly-daily"] = (
        DAILY_SCHEMA, daily, validate_daily,
        "One night's daily report record, daily.json (ADR-0269): the build, its stable "
        "baseline, the verified core verdict per instance with exact rationals and their "
        "three-significant-digit text, the telemetry figures, the trend of the headline "
        "instances across nights, what changed since the previous night, the GitHub backlog, "
        "the machine, and which sections are available and why not.")
    c.CONTRACTS["nightly-publish"] = (
        PUBLISH_SCHEMA, receipt, validate_receipt,
        "What publishing one night did, builds_root/<date>/publish.json (ADR-0270): the "
        "pre-release of its tag with each asset's name, size and digest as GitHub reports "
        "them, or nothing for a skipped night; the Telegram message id or why there is none; "
        "the nightly pre-releases pruned, each whose release went but whose tag deletion "
        "failed, and the nightly-named releases refused as foreign; "
        "how many times the night was amended and when.")
