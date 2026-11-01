#!/usr/bin/env python3
"""Publish a built nightly as a GitHub pre-release, tell the owner, prune old nights (ADR-0272).

    python3 -I scripts/nightly/publish.py publish --config PATH
                                                  [--date YYYY-MM-DD | --night-of UNIT]
                                                  [--report DIR | --report-root ROOT]
                                                  [--notify-again]

Without --date the night is the UTC date systemd last started UNIT, or else today's UTC date;
without --report the report is the newest of ROOT/DATE and ROOT/DATE.<k> (the highest k), ROOT
being --report-root or the configuration's reports_root (ADR-0273); every default is printed
on stderr. For a `built` night (builds_root/<date>/nightly.json) `publish` verifies the night
as `build` left it, creates the pre-release of tag nightly-YYYY.MM.DD at its head_sha with
digest.md as its notes, uploads the two archives, SHA256SUMS, SHA256SUMS.minisig,
nightly.json, daily.json and report.html, sends digest.md as one plain-text Telegram message, prunes the nightly
pre-releases beyond the newest `keep_nights`, writes builds_root/<date>/publish.json
(af.nightly-publish/1) and prints the release URL. A later `publish` of the night amends it:
the shipped archives, sums, signature and record must be the ones on the release, and only the
notes, daily.json and report.html are replaced. A `skipped` night publishes nothing and sends
one line. docs/nightly.md is the operator page.

Exit codes: 0 published, 1 refused with the reason on stderr, or published with a Telegram
message or a pruning request that failed or a foreign nightly-named release left alone (the
receipt is written).
"""
import argparse
import hashlib
import http.client
import json
import os
import re
import subprocess
import sys
import time

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(ROOT, "scripts", "performance"))
sys.path.insert(0, HERE)

import nightlychain  # noqa: E402
from githubrest import GitHub, Refused, https_send  # noqa: E402
from perflab import contracts, tomlfile  # noqa: E402
from perflab.nightlycontracts import (KEEP_NIGHTS_DEFAULT, REPORT_FILES, SIGNATURE,  # noqa: E402
                                      SUMS, TAG_PATTERN, release_assets)
from perflab.values import ContractError  # noqa: E402

EXIT_PUBLISHED, EXIT_REFUSED = 0, 1
DIGEST_BYTES_MAX = 3500
REPORT_BYTES_MAX = 4 * 1024 * 1024
SIGN_SECONDS = 120
TELEGRAM_API = "https://api.telegram.org"
TELEGRAM_RESPONSE_MAX = 64 * 1024
CONTENT_TYPES = (("tar.gz", "application/gzip"), (".json", "application/json"),
                 (".html", "text/html; charset=utf-8"), (".minisig", "text/plain"),
                 ("SHA256SUMS", "text/plain"))


class Failed(Exception):
    """The night is published, but one step after it failed; the receipt is still written."""


# Configuration and secrets ------------------------------------------------------------------

def load_config(path):
    """The af.nightly-config/1 TOML at `path`, validated; its `[publish]` table is required."""
    try:
        with open(path, "rb") as stream:
            data = stream.read(tomlfile.MAX_TOML_BYTES + 1)
    except OSError as error:
        raise Refused("--config %s: %s" % (path, error))
    doc = contracts.validate_nightly_config(tomlfile.loads(data, path), path)
    if "publish" not in doc:
        raise Refused("%s has no [publish] table: publishing needs its write token" % path)
    return doc


def read_secret(path, key, pattern):
    """The token in the file `path` names; never printed, only the key and path are."""
    try:
        with open(path, "r", encoding="utf-8") as stream:
            token = stream.read(4096).strip()
    except (OSError, UnicodeDecodeError) as error:
        raise Refused("%s %s: %s" % (key, path, getattr(error, "strerror", None)
                                     or type(error).__name__))
    if not token or re.match(pattern, token) is None:
        raise Refused("%s %s holds no token" % (key, path))
    return token


# The night and its report -------------------------------------------------------------------

def sha256(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def read_bounded(path, limit, what):
    """The bytes of the regular file `path`, at least one and at most `limit`, or refused."""
    if not os.path.isfile(path):
        raise Refused("%s %s is missing" % (what, path))
    with open(path, "rb") as stream:
        data = stream.read(limit + 1)
    if not data or len(data) > limit:
        raise Refused("%s %s is %s" % (what, path, "larger than %d bytes" % limit if data
                                       else "empty"))
    return data


def read_report(directory, record, repository):
    """{name: bytes} of this night's report and its digest.md text, each checked.

    A schema-valid report of another night is not a recovery input: its identity must match
    the selected build before any GitHub or Telegram write can follow.
    """
    path = os.path.join(directory, "daily.json")
    daily = read_bounded(path, REPORT_BYTES_MAX, "the report's daily.json")
    try:
        doc = contracts.read_json(daily, "nightly-daily", path)
    except ContractError as error:
        raise Refused(str(error))
    expected = (("date", doc.get("date"), record["date"]),
                ("tag", doc.get("tag"), record["tag"]),
                ("repository", doc.get("repository"), repository),
                ("build.head_sha", doc.get("build", {}).get("head_sha"), record["head_sha"]))
    mismatches = ["%s is %r, not the night's %r" % (name, found, wanted)
                  for name, found, wanted in expected if found != wanted]
    if mismatches:
        raise Refused("the report %s does not belong to the night %s: %s"
                      % (directory, record["date"], "; ".join(mismatches)))
    html = read_bounded(os.path.join(directory, "report.html"), REPORT_BYTES_MAX,
                        "the report's report.html")
    digest = read_bounded(os.path.join(directory, "digest.md"), DIGEST_BYTES_MAX,
                          "the report's digest.md")
    try:
        text = digest.decode("utf-8")
    except UnicodeDecodeError:
        raise Refused("the report's digest.md is not UTF-8 text")
    if not text.strip():
        raise Refused("the report's digest.md is blank")
    return {"daily.json": daily, "report.html": html}, text


def newest_report(root, date):
    """The newest report directory of the night below `root`: of `<date>` and `<date>.<k>`
    (k >= 2), the one with the highest k, as `report.py compose --output-root` names them; None
    when there is none. A link is never one."""
    if not os.path.isdir(root):
        raise Refused("--report-root %s is not a directory" % root)
    pattern = re.compile(r"^%s(?:\.([2-9]|[1-9][0-9]+))?$" % re.escape(date))
    found = []
    for name in os.listdir(root):
        match = pattern.match(name)
        path = os.path.join(root, name)
        if match is not None and os.path.isdir(path) and not os.path.islink(path):
            found.append((int(match.group(1) or 1), path))
    return max(found)[1] if found else None


def read_night(builds_root, date):
    path = os.path.join(builds_root, date, "nightly.json")
    if not os.path.isfile(path):
        raise Refused("%s is missing: the night %s was never built" % (path, date))
    record = contracts.read_file(path, "nightly-build")
    if record["date"] != date:
        raise Refused("%s records the night %s" % (path, record["date"]))
    return record


def verify_night(config, record, run):
    """{asset name: bytes} of the night's shipped files, in upload order, once each matches
    what `build` recorded and SHA256SUMS.minisig verifies with `public_key`."""
    night = os.path.join(config["builds_root"], record["date"])
    files = {}
    sums = []
    for entry in record["targets"]:
        name = os.path.basename(entry["asset"])
        data = read_bounded(os.path.join(night, entry["asset"]), 2 ** 40, "the archive")
        if (sha256(data), len(data)) != (entry["sha256"], entry["bytes"]):
            raise Refused("%s is not the archive nightly.json records (%s, %d bytes)"
                          % (entry["asset"], entry["sha256"], entry["bytes"]))
        files[name] = data
        sums.append((name, entry["sha256"][len("sha256:"):]))
    written = read_bounded(os.path.join(night, SUMS), 64 * 1024, "the night's SHA256SUMS")
    expected = "".join("%s  %s\n" % (digest, name) for name, digest in sorted(sums))
    if written != expected.encode("ascii"):
        raise Refused("%s does not match the night's archives" % SUMS)
    signature = read_bounded(os.path.join(night, SIGNATURE), 64 * 1024,
                             "the night's SHA256SUMS.minisig")
    verified = run(["minisign", "-V", "-q", "-m", "SHA256SUMS", "-x", "SHA256SUMS.minisig",
                    "-p", config["public_key"]], os.path.join(night, "dist"))
    if verified.returncode != 0:
        raise Refused("SHA256SUMS.minisig does not verify against public_key %s: %s" % (
            config["public_key"], verified.stderr.decode("utf-8", "replace").strip()[-300:]))
    lines = signature.decode("utf-8", "replace").splitlines()
    comment = "af " + record["tag"]
    if len(lines) < 3 or lines[2] != "trusted comment: " + comment:
        raise Refused("SHA256SUMS.minisig does not carry the trusted comment %r" % comment)
    files["SHA256SUMS"] = written
    files["SHA256SUMS.minisig"] = signature
    files["nightly.json"] = read_bounded(os.path.join(night, "nightly.json"), 2 ** 24,
                                         "the night's nightly.json")
    return files


def run_command(argv, cwd):
    """One bounded command, its environment only PATH and LC_ALL."""
    try:
        return subprocess.run(argv, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                                   "LC_ALL": "C"}, timeout=SIGN_SECONDS)
    except subprocess.TimeoutExpired:
        raise Refused("%s did not finish within %d s" % (argv[0], SIGN_SECONDS))
    except OSError as error:
        raise Refused("%s: %s" % (argv[0], error))


# Telegram -----------------------------------------------------------------------------------

class Telegram:
    """One plain-text sendMessage of the Bot API through `send`; the token is in the URL the
    API requires and in nothing printed."""

    def __init__(self, token, chat_id, send=https_send):
        self.token = token
        self.chat_id = chat_id
        self.send = send

    def message(self, text):
        """The id of the message sent, or Failed with the reason."""
        chat = int(self.chat_id) if re.match(r"^-?[0-9]+$", self.chat_id) else self.chat_id
        body = json.dumps({"chat_id": chat, "text": text,
                           "link_preview_options": {"is_disabled": True}}).encode("utf-8")
        try:
            status, answer = self.send("POST", "%s/bot%s/sendMessage" % (TELEGRAM_API,
                                                                        self.token),
                                       {"Content-Type": "application/json",
                                        "User-Agent": "af-nightly-publish"}, body)
        except (OSError, http.client.HTTPException) as error:
            reason = getattr(error, "reason", None)
            raise Failed("Telegram sendMessage failed: %s" % (
                str(reason if reason is not None else error) or type(error).__name__))
        try:
            doc = json.loads(answer[:TELEGRAM_RESPONSE_MAX].decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            doc = None
        result = doc.get("result") if isinstance(doc, dict) else None
        if status == 200 and isinstance(result, dict) and doc.get("ok") is True \
                and type(result.get("message_id")) is int and result["message_id"] > 0:
            return result["message_id"]
        described = doc.get("description") if isinstance(doc, dict) else None
        raise Failed("Telegram sendMessage failed: HTTP %d%s" % (
            status, ": " + described[:200] if isinstance(described, str) else ""))


def telegram_of(config, send):
    publish = config["publish"]
    if "telegram_token_file" not in publish:
        return None
    token = read_secret(publish["telegram_token_file"], "telegram_token_file",
                        r"^[0-9]+:[A-Za-z0-9_-]+$")
    return Telegram(token, publish["telegram_chat_id"], send)


UNCONFIGURED = "not sent: [publish] configures no telegram_token_file and telegram_chat_id"


def notify(telegram, text):
    """(telegram, telegram_reason, failure) of sending `text`."""
    if telegram is None:
        return None, UNCONFIGURED, None
    try:
        return {"message_id": telegram.message(text)}, None, None
    except Failed as error:
        return None, str(error), "nightly: failed: %s" % error


# The release --------------------------------------------------------------------------------

def content_type(name):
    for suffix, kind in CONTENT_TYPES:
        if name.endswith(suffix):
            return kind
    return "application/octet-stream"


def reported(name, found, data):
    """The receipt entry of the asset GitHub reports for `name`, which must carry `data`'s
    size and digest."""
    if found["name"] != name or found["state"] != "uploaded":
        raise Refused("GitHub reports %s as %r in state %s" % (name, found["name"],
                                                               found["state"]))
    if found["digest"] is None:
        raise Refused("GitHub reports no digest for %s" % name)
    if (found["size"], found["digest"]) != (len(data), sha256(data)):
        raise Refused("GitHub reports %s with %d bytes and %s, not the night's %d bytes and %s"
                      % (name, found["size"], found["digest"], len(data), sha256(data)))
    return {"name": name, "size": found["size"], "digest": found["digest"]}


def check_release_location(release, record, repository):
    """Bind a found release's tag and upload target before an amendment can write."""
    tag = record["tag"]
    if release.get("tag_name") != tag:
        raise Refused("GitHub answered the release of %s with tag %s"
                      % (tag, release.get("tag_name")))
    expected = ("https://uploads.github.com/repos/%s/releases/%s/assets{?name,label}"
                % (repository, release.get("id")))
    if release.get("upload_url") != expected:
        raise Refused("GitHub's upload URL for %s does not name its own release id %s"
                      % (tag, release.get("id")))


def check_release_response(release, record, repository):
    """Bind every create/find/update answer to this night and its own release before writes."""
    check_release_location(release, record, repository)
    tag = record["tag"]
    if (release.get("target_commitish"), release.get("prerelease"),
            release.get("draft")) != (record["head_sha"], True, False):
        raise Refused("GitHub holds the release of %s as something else (a draft, another "
                      "commit or not a pre-release); nothing was uploaded" % tag)


def check_existing(release, record, shipped):
    """Refuse, before anything is written, a release that is not this night's: not a
    pre-release, at another commit, or carrying shipped bytes that differ from the night's."""
    if not release["prerelease"] or release["draft"]:
        raise Refused("the release of %s is not a published pre-release: a foreign release, "
                      "never amended" % record["tag"])
    if release["target_commitish"] != record["head_sha"]:
        raise Refused("the release of %s targets %s, not the night's head %s" % (
            record["tag"], release["target_commitish"], record["head_sha"]))
    by_name = dict((item["name"], item) for item in release["assets"])
    for name, data in shipped.items():
        if name in by_name:
            reported(name, by_name[name], data)


def publish_release(github, record, shipped, report, notes):
    """(release, receipt assets, amended) after creating or amending the night's release."""
    tag = record["tag"]
    commit = github.tag_commit(tag)
    if commit is not None and commit != record["head_sha"]:
        raise Refused("the tag %s names %s on GitHub, not the night's head %s"
                      % (tag, commit, record["head_sha"]))
    release = github.release_by_tag(tag)
    amended = release is not None
    if amended:
        check_release_location(release, record, github.repository)
        check_existing(release, record, shipped)
        release = github.update_notes(release["id"], notes)
    else:
        release = github.create_release(tag, record["head_sha"],
                                        "Nightly " + record["date"], notes)
    # What GitHub answered, created or updated, is the release the assets go to: its tag,
    # commit, prerelease state and upload URL are all bound before an asset write.
    check_release_response(release, record, github.repository)
    by_name = dict((item["name"], item) for item in release["assets"])
    files = dict(shipped, **report)
    assets = []
    for name in release_assets(tag):
        found = by_name.get(name)
        if found is not None and name in REPORT_FILES:
            github.delete_asset(found["id"])
            found = None
        if found is None:
            found = github.upload(release["upload_url"], name, files[name], content_type(name))
        assets.append(reported(name, found, files[name]))
    return release, assets, amended


def prune(github, keep, own, pruned, failures, foreign):
    """Delete the nightly pre-releases beyond the newest `keep` by tag date, oldest first, each
    release before its tag, the night's own release included when it is that old. `foreign`
    gets, before anything is deleted, the nightly-named releases that are drafts or not
    pre-releases (other than the night's own): they are left alone and named, whatever happens
    after. `pruned` gets each tag once its tag ref is deleted too (a ref GitHub no longer holds
    counts: nothing of the night remains); `failures` gets the tag and reason of a release that
    went while its tag deletion was refused, and pruning stops there. Nothing else is
    touched."""
    nightly, others = [], []
    for item in github.releases():
        if re.match(TAG_PATTERN, item["tag_name"]) is None:
            continue
        (nightly if item["prerelease"] and not item["draft"] else others).append(item)
    foreign.extend(sorted(item["tag_name"] for item in others if item["tag_name"] != own))
    tags = [item["tag_name"] for item in nightly]
    if len(set(tags)) != len(tags):
        raise Refused("two nightly pre-releases share a tag; prune by hand")
    nightly.sort(key=lambda item: item["tag_name"], reverse=True)
    for item in reversed(nightly[keep:]):
        github.delete_release(item["id"])
        try:
            github.delete_tag(item["tag_name"])
        except Refused as error:
            failures.append({"tag": item["tag_name"], "reason": str(error)[:1024]})
            raise Refused("the release of %s is deleted but its tag is not; delete the tag by "
                          "hand: %s" % (item["tag_name"], error))
        pruned.append(item["tag_name"])


# The receipt --------------------------------------------------------------------------------

def previous_receipt(path, record):
    if not os.path.lexists(path):
        return None
    receipt = contracts.read_file(path, "nightly-publish")
    if (receipt["date"], receipt["head_sha"], receipt["status"]) != (
            record["date"], record["head_sha"],
            "published" if record["status"] == "built" else "skipped"):
        raise Refused("%s records another publication of the night" % path)
    return receipt


def write_receipt(path, receipt):
    data = contracts.write_bytes(receipt, "nightly-publish")
    with open(path + ".tmp", "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(path + ".tmp", path)


def publish(config, args, github, telegram_send, run, clock):
    """(receipt, failure lines) of publishing the night; refused before the receipt
    otherwise. Every input, the tokens included, is read before the first request."""
    record = read_night(config["builds_root"], args.date)
    telegram = telegram_of(config, telegram_send)
    night = os.path.join(config["builds_root"], args.date)
    path = os.path.join(night, "publish.json")
    previous = previous_receipt(path, record)
    receipt = {"schema": contracts.CONTRACTS["nightly-publish"][0], "date": record["date"],
               "tag": record["tag"], "head_sha": record["head_sha"],
               "repository": config["repository"], "pruned": [], "prune_failures": [],
               "foreign": []}
    failures = []
    if record["status"] == "skipped":
        receipt.update(status="skipped", release=None, assets=[])
        amended = previous is not None
        text = "%s: no new commits on main since %s" % (record["tag"], record["previous_tag"])
    else:
        if args.report is None:
            raise Refused("--report-root %s holds no report of the night %s (%s or %s.<k>): "
                          "compose it first" % (args.report_root, args.date, args.date,
                                                args.date))
        report, text = read_report(args.report, record, config["repository"])
        shipped = verify_night(config, record, run)
        if github is None:
            github = GitHub(config["repository"], read_secret(
                config["publish"]["github_token_file"], "[publish] github_token_file",
                r"^[A-Za-z0-9_.-]+$"))
        release, assets, amended = publish_release(github, record, shipped, report, text)
        receipt.update(status="published", assets=assets,
                       release={"id": release["id"], "url": release["html_url"]})
    # A night with a receipt had its message, or the reason there is none: a skipped night
    # never sends again, an amendment only with --notify-again. A retry of a first publish
    # that died before its receipt finds the release and sends the message: once when the
    # first died during an upload, a second time when it died after the message and before
    # the receipt, the receipt being the only memory of a message.
    if previous is not None and (record["status"] == "skipped"
                                 or amended and not args.notify_again):
        receipt.update(telegram=previous["telegram"],
                       telegram_reason=previous["telegram_reason"])
    else:
        sent, reason, failure = notify(telegram, text)
        receipt.update(telegram=sent, telegram_reason=reason)
        if failure:
            failures.append(failure)
    if record["status"] == "built":
        # The release and the message are out: a pruning failure is reported beside them.
        pruned, prune_failures, foreign = [], [], []
        try:
            prune(github, config["publish"].get("keep_nights", KEEP_NIGHTS_DEFAULT),
                  record["tag"], pruned, prune_failures, foreign)
        except Refused as error:
            failures.append("nightly: failed: pruning: %s" % error)
        receipt.update(pruned=sorted(pruned), prune_failures=prune_failures, foreign=foreign)
        failures += ["nightly: refused: %s is a nightly-named release that is a draft or not "
                     "a pre-release: foreign, never pruned" % tag for tag in foreign]
    receipt.update(amendments=(previous["amendments"] + 1 if previous is not None else 1)
                   if amended else 0, published_unix_ms=clock())
    write_receipt(path, receipt)
    return receipt, failures


# Command line -------------------------------------------------------------------------------

class Parser(argparse.ArgumentParser):
    def error(self, message):
        self.print_usage(sys.stderr)
        sys.stderr.write("nightly: refused: %s\n" % message)
        sys.exit(EXIT_REFUSED)


def parse(argv):
    parser = Parser(prog="publish.py", description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", parser_class=Parser)
    commands.required = True
    command = commands.add_parser("publish", help="publish the night's pre-release, send its "
                                  "digest and prune old nightlies")
    command.add_argument("--config", required=True, help="the af.nightly-config/1 TOML with "
                         "its [publish] table")
    nightlychain.add_night_options(command)
    directory = command.add_mutually_exclusive_group()
    directory.add_argument("--report", help="the report directory: daily.json, report.html and "
                           "digest.md (a skipped night reads none of them)")
    directory.add_argument("--report-root", help="the newest of ROOT/<date> and "
                           "ROOT/<date>.<k> is the report directory (default: the "
                           "configuration's reports_root)")
    command.add_argument("--notify-again", action="store_true", help="send the Telegram "
                         "message again when amending a built night; a skipped night's line "
                         "is never sent twice")
    return parser.parse_args(argv)


def main(argv=None, github=None, telegram_send=https_send, run=run_command,
         clock=lambda: int(time.time() * 1000), show=nightlychain.systemctl_show):
    args = parse(argv)
    try:
        args.date, chosen = nightlychain.night(args, clock, show, Refused)
        if chosen:
            sys.stderr.write(chosen)
        config = load_config(args.config)
        if args.report is None:
            if args.report_root is None:
                if "reports_root" not in config:
                    raise Refused("publish needs --report, --report-root or the "
                                  "configuration's reports_root")
                args.report_root = config["reports_root"]
                sys.stderr.write("nightly: --report-root %s (the configuration's "
                                 "reports_root)\n" % args.report_root)
            args.report = newest_report(args.report_root, args.date)
            if args.report is not None:
                sys.stderr.write("nightly: --report %s (the newest of the night under %s)\n"
                                 % (args.report, args.report_root))
        receipt, failures = publish(config, args, github, telegram_send, run, clock)
    except (Refused, ContractError) as error:
        sys.stderr.write("nightly: refused: %s\n" % error)
        return EXIT_REFUSED
    if receipt["release"] is not None:
        sys.stdout.write(receipt["release"]["url"] + "\n")
    if receipt["tag"] in receipt["pruned"]:
        sys.stderr.write("nightly: %s is older than the newest keep_nights nightly "
                         "pre-releases: published, then pruned\n" % receipt["tag"])
    elif receipt["release"] is None:
        sys.stderr.write("nightly: %s was skipped; published nothing\n" % receipt["tag"])
    for failure in failures:
        sys.stderr.write(failure + "\n")
    return EXIT_REFUSED if failures else EXIT_PUBLISHED


if __name__ == "__main__":
    sys.exit(main())
