"""Closed contracts of the owner's nightly build (ADR-0265): its configuration, the plan it
prints and the record each night leaves in its directory.

A nightly is a separate train from the releases: its tag is `nightly-YYYY.MM.DD` and its
version `<core>-nightly.<YYYYMMDD>`, both derived here from the night's date and the
workspace version on the chosen commit, so a reader recomputes them instead of trusting them.
"""
import datetime
import re

from .spec import Arr, Const, Field, Int, Map, Nullable, Obj, Str
from .values import ContractError, require

CONFIG_SCHEMA = "af.nightly-config/1"
PLAN_SCHEMA = "af.nightly-plan/1"
BUILD_SCHEMA = "af.nightly-build/1"

# The Linux targets a nightly ships, in the order they are built and recorded. The VM cannot
# build macOS, so a nightly has no aarch64-apple-darwin archive.
TARGETS = ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl")
SUMS = "dist/SHA256SUMS"
SIGNATURE = "dist/SHA256SUMS.minisig"
DATE_PATTERN = r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$"
TAG_PATTERN = r"^nightly-[0-9]{4}\.[0-9]{2}\.[0-9]{2}$"
VERSION_PATTERN = r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-nightly\.[0-9]{8}$"
# The workspace version a nightly is derived from: plain SemVer, no build metadata.
_WORKSPACE_VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
                                r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$")
# Jobs of the release workflow that run on a main commit beside its `check / …` jobs; a
# nightly's greenness never depends on them, so a configuration cannot require them.
RELEASE_JOBS = ("resolve the release", "sign and publish", "pin the release in .af/af.lock")
RELEASE_JOB_PREFIX = "build "
CHECK_PREFIX = "check /"


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
        Field("github_token_file", path, required=False))
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

    def validate_config(doc, where="nightly.toml"):
        configuration.validate(doc, where)
        for name in doc["required_checks"]:
            require(name not in RELEASE_JOBS and not name.startswith(RELEASE_JOB_PREFIX),
                    "%s.required_checks: %r is a job of the release workflow, which a "
                    "nightly never waits for" % (where, name))
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

    c.validate_nightly_config = validate_config
    c.validate_nightly_plan = validate_plan
    c.validate_nightly_build = validate_record
    c.CONTRACTS["nightly-config"] = (
        CONFIG_SCHEMA, configuration, validate_config,
        "The owner's nightly build configuration as parsed TOML (ADR-0265): the repository, "
        "the bare mirror and builds root, the pinned musl build image, the aarch64 user-mode "
        "runner, the nightly signing key pair, the check runs a green commit must carry and "
        "how far back the walk may go.")
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
