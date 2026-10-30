"""Publishing a nightly (scripts/nightly/publish.py, ADR-0271) with injected fakes.

A night is built in a temporary builds root from the nightly-build fixture; GitHub is an
in-memory repository behind the transport's `send`, Telegram a recorder of `sendMessage`, and
minisign a runner that answers as told. No test reaches the network, runs Docker or cargo.
"""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import re
import subprocess
import sys
import urllib.parse

from perflab import contracts
from perflab.values import ContractError
from tests.support import ROOT, TempDir, read
from tests.test_nightly_build import nightly as build

sys.path.insert(0, os.path.join(ROOT, "scripts", "nightly"))
import githubrest  # noqa: E402

PUBLISH = os.path.join(ROOT, "scripts", "nightly", "publish.py")
_spec = importlib.util.spec_from_file_location("nightly_publish", PUBLISH)
publish = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(publish)

REPO = "owner/af"
HEAD = "b" * 40
WRITE_TOKEN = "ghp_writeTOKEN0123456789"
BOT_TOKEN = "123456:bot-SECRET_token"
TAG = "nightly-2026.10.29"
DIGEST = "nightly-2026.10.29: 2 commits, all cases within noise\n"
ASSETS = ["af-nightly-2026.10.29-x86_64-unknown-linux-musl.tar.gz",
          "af-nightly-2026.10.29-aarch64-unknown-linux-musl.tar.gz", "SHA256SUMS",
          "SHA256SUMS.minisig", "nightly.json", "daily.json", "report.html"]


def sha256(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


class FakeGitHub:
    """One repository's releases, assets and tag refs, as the REST API answers for them."""

    def __init__(self, test):
        self.test = test
        self.releases = {}
        self.tags = {}
        self.requests = []
        self.next_id = 1000
        self.fail = None
        # When set, a created release is answered as a draft, whatever was asked.
        self.create_draft = False

    def id(self):
        self.next_id += 1
        return self.next_id

    def add(self, tag, prerelease=True, sha=HEAD, assets=(), draft=False):
        release = {"id": self.id(), "tag_name": tag, "target_commitish": sha,
                   "prerelease": prerelease, "draft": draft, "name": tag, "body": "",
                   "assets": []}
        for name, data in assets:
            release["assets"].append(self.asset(name, data))
        self.releases[release["id"]] = release
        self.tags[tag] = sha
        return release

    def asset(self, name, data):
        return {"id": self.id(), "name": name, "size": len(data), "digest": sha256(data),
                "state": "uploaded", "data": data}

    def render(self, release):
        out = dict((key, value) for key, value in release.items() if key != "assets")
        out["html_url"] = "https://github.com/%s/releases/tag/%s" % (REPO, release["tag_name"])
        out["upload_url"] = ("https://uploads.github.com/repos/%s/releases/%d/assets{?name,label}"
                             % (REPO, release["id"]))
        out["assets"] = [dict((k, v) for k, v in item.items() if k != "data")
                         for item in release["assets"]]
        return out

    def by_tag(self, tag):
        found = [r for r in self.releases.values() if r["tag_name"] == tag]
        return found[0] if found else None

    def writes(self):
        return [(method, path) for method, path in self.requests if method != "GET"]

    def send(self, method, url, headers, body):
        self.test.assertEqual(headers["Authorization"], "Bearer " + WRITE_TOKEN)
        parts = urllib.parse.urlsplit(url)
        self.test.assertIn(parts.netloc, ("api.github.com", "uploads.github.com"))
        path = parts.path + ("?" + parts.query if parts.query else "")
        self.requests.append((method, path))
        failed = self.fail(method, path) if self.fail else False
        if failed:
            # True answers 502; an int is the status to answer with.
            status = failed if isinstance(failed, int) and not isinstance(failed, bool) else 502
            return status, b'{"message": "%s"}' % (b"Bad gateway" if status == 502
                                                    else b"Validation Failed")
        base = "/repos/%s" % REPO
        doc = json.loads(body) if body and headers.get("Content-Type") == "application/json" \
            else None
        match = re.match(r"^%s/git/ref/tags/(.+)$" % base, path)
        if method == "GET" and match:
            sha = self.tags.get(match.group(1))
            if sha is None:
                return 404, b'{"message": "Not Found"}'
            return 200, json.dumps({"ref": "refs/tags/" + match.group(1),
                                    "object": {"type": "commit", "sha": sha}}).encode()
        match = re.match(r"^%s/releases/tags/(.+)$" % base, path)
        if method == "GET" and match:
            release = self.by_tag(match.group(1))
            if release is None:
                return 404, b'{"message": "Not Found"}'
            return 200, json.dumps(self.render(release)).encode()
        if method == "POST" and path == base + "/releases":
            if self.by_tag(doc["tag_name"]):
                return 422, b'{"message": "Validation Failed"}'
            self.test.assertEqual((doc["draft"], doc["prerelease"], doc["make_latest"]),
                                  (False, True, "false"))
            release = self.add(doc["tag_name"], sha=self.tags.get(doc["tag_name"],
                                                                   doc["target_commitish"]),
                               draft=self.create_draft)
            release.update(name=doc["name"], body=doc["body"],
                           target_commitish=doc["target_commitish"])
            return 201, json.dumps(self.render(release)).encode()
        match = re.match(r"^%s/releases/([0-9]+)$" % base, path)
        if match and method == "PATCH":
            release = self.releases[int(match.group(1))]
            release.update(doc)
            return 200, json.dumps(self.render(release)).encode()
        if match and method == "DELETE":
            del self.releases[int(match.group(1))]
            return 204, b""
        match = re.match(r"^%s/releases/([0-9]+)/assets\?name=(.+)$" % base, path)
        if match and method == "POST":
            self.test.assertEqual(parts.netloc, "uploads.github.com")
            self.test.assertTrue(headers["Content-Type"])
            release = self.releases[int(match.group(1))]
            name = urllib.parse.unquote(match.group(2))
            if any(item["name"] == name for item in release["assets"]):
                return 422, b'{"message": "already_exists"}'
            item = self.asset(name, body)
            release["assets"].append(item)
            return 201, json.dumps(dict((k, v) for k, v in item.items() if k != "data")).encode()
        match = re.match(r"^%s/releases/assets/([0-9]+)$" % base, path)
        if match and method == "DELETE":
            for release in self.releases.values():
                release["assets"] = [item for item in release["assets"]
                                     if item["id"] != int(match.group(1))]
            return 204, b""
        match = re.match(r"^%s/releases\?per_page=([0-9]+)&page=([0-9]+)$" % base, path)
        if match and method == "GET":
            size, page = int(match.group(1)), int(match.group(2))
            listed = sorted(self.releases.values(), key=lambda r: r["id"], reverse=True)
            batch = listed[(page - 1) * size:page * size]
            return 200, json.dumps([self.render(r) for r in batch]).encode()
        match = re.match(r"^%s/git/refs/tags/(.+)$" % base, path)
        if match and method == "DELETE":
            if self.tags.pop(match.group(1), None) is None:
                return 422, b'{"message": "Reference does not exist"}'
            return 204, b""
        raise AssertionError("unexpected GitHub request %s %s" % (method, path))

    def client(self):
        return githubrest.GitHub(REPO, WRITE_TOKEN, send=self.send)


class FakeTelegram:
    def __init__(self, test, answer=None):
        self.test = test
        self.sent = []
        self.answer = answer

    def send(self, method, url, headers, body):
        self.test.assertEqual((method, url), ("POST", "https://api.telegram.org/bot%s/sendMessage"
                                              % BOT_TOKEN))
        self.sent.append(json.loads(body))
        if self.answer is not None:
            return self.answer
        return 200, json.dumps({"ok": True, "result": {"message_id": 70 + len(self.sent)}}
                               ).encode()


class Runner:
    """minisign -V, verifying or not."""

    def __init__(self, verifies=True):
        self.verifies = verifies
        self.calls = []

    def __call__(self, argv, cwd):
        self.calls.append((argv, cwd))
        return subprocess.CompletedProcess(argv, 0 if self.verifies else 1, b"",
                                           b"" if self.verifies else
                                           b"Signature verification failed\n")


class Night(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.builds = self.path("builds")
        self.night = os.path.join(self.builds, "2026-10-29")
        for name, text in (("github-write", WRITE_TOKEN), ("telegram", BOT_TOKEN),
                           ("nightly.pub", "untrusted comment: key\nRWQ...\n")):
            with open(self.path(name), "w") as stream:
                stream.write(text + "\n")
        self.config = self.path("nightly.toml")
        self.write_config()
        self.github = FakeGitHub(self)
        self.telegram = FakeTelegram(self)
        self.runner = Runner()

    def write_config(self, telegram=True, **publish_values):
        values = {"github_token_file": self.path("github-write")}
        if telegram:
            values.update(telegram_token_file=self.path("telegram"),
                          telegram_chat_id="-1001234567890")
        values.update(publish_values)
        top = {"repository": REPO, "mirror": self.path("mirror.git"),
               "builds_root": self.builds, "build_image": "sha256:" + "1" * 64,
               "qemu_aarch64": self.path("qemu"), "sign_key": self.path("nightly.key"),
               "public_key": self.path("nightly.pub"), "required_checks": ["check / lint"]}
        lines = ['schema = "af.nightly-config/1"']
        lines += ["%s = %s" % item for item in ((k, json.dumps(v)) for k, v in top.items())]
        lines.append("[publish]")
        lines += ["%s = %s" % (k, json.dumps(v)) for k, v in values.items()]
        with open(self.config, "w") as stream:
            stream.write("\n".join(lines) + "\n")

    def build(self, date="2026-10-29"):
        """A built night in builds_root/<date>/: the fixture's record over real files."""
        night = os.path.join(self.builds, date)
        os.makedirs(os.path.join(night, "dist"))
        record = json.loads(read(os.path.join(
            ROOT, "fixtures/performance/schema/nightly-build/valid-built.json")))
        tag = build.nightly_tag(date)
        record.update(date=date, tag=tag, version=build.nightly_version("0.14.0-rc.3", date))
        sums = {}
        for entry in record["targets"]:
            data = ("archive of %s for %s\n" % (entry["target"], date)).encode()
            entry["asset"] = "dist/" + build.asset_name(tag, entry["target"])
            with open(os.path.join(night, entry["asset"]), "wb") as stream:
                stream.write(data)
            entry.update(sha256=sha256(data), bytes=len(data))
            sums[os.path.basename(entry["asset"])] = sha256(data)[len("sha256:"):]
        with open(os.path.join(night, "dist", "SHA256SUMS"), "w") as stream:
            stream.write(build.sha256sums_text(sums))
        with open(os.path.join(night, "dist", "SHA256SUMS.minisig"), "w") as stream:
            stream.write("untrusted comment: signature\nRW...\ntrusted comment: af %s\nsig\n"
                         % tag)
        with open(os.path.join(night, "nightly.json"), "wb") as stream:
            stream.write(contracts.write_bytes(record, "nightly-build"))
        return record

    def skip(self, date="2026-10-30"):
        record = json.loads(read(os.path.join(
            ROOT, "fixtures/performance/schema/nightly-build/valid-skipped.json")))
        record.update(date=date, tag=build.nightly_tag(date),
                      version=build.nightly_version("0.14.0-rc.3", date))
        os.makedirs(os.path.join(self.builds, date))
        with open(os.path.join(self.builds, date, "nightly.json"), "wb") as stream:
            stream.write(contracts.write_bytes(record, "nightly-build"))
        return record

    def report(self, name="report", daily=None, html=b"<html>report</html>\n", digest=DIGEST):
        directory = self.path(name)
        os.makedirs(directory, exist_ok=True)
        files = {"daily.json": daily if daily is not None else json.dumps(
            {"schema": "af.nightly-daily/1", "date": "2026-10-29"}).encode(),
            "report.html": html, "digest.md": digest.encode() if isinstance(digest, str)
            else digest}
        for key, data in files.items():
            if data is None:
                continue
            with open(os.path.join(directory, key), "wb") as stream:
                stream.write(data)
        return directory

    def publish(self, *extra, date="2026-10-29", report=None):
        out, err = io.StringIO(), io.StringIO()
        argv = ["publish", "--config", self.config, "--date", date,
                "--report", report or self.report()] + list(extra)
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = publish.main(argv, github=self.github.client(),
                                telegram_send=self.telegram.send, run=self.runner,
                                clock=lambda: 1761710400000)
        for text in (out.getvalue(), err.getvalue()):
            self.assertNotIn(WRITE_TOKEN, text)
            self.assertNotIn(BOT_TOKEN, text)
        return code, out.getvalue(), err.getvalue()

    def receipt(self, date="2026-10-29"):
        path = os.path.join(self.builds, date, "publish.json")
        data = read(path)
        self.assertNotIn(WRITE_TOKEN.encode(), data)
        self.assertNotIn(BOT_TOKEN.encode(), data)
        return contracts.read_file(path, "nightly-publish")


class FirstPublish(Night):
    def test_a_built_night_becomes_one_pre_release_with_seven_assets_and_one_message(self):
        record = self.build()
        code, out, err = self.publish()
        self.assertEqual(code, 0, err)
        url = "https://github.com/owner/af/releases/tag/" + TAG
        self.assertEqual(out, url + "\n")
        self.assertNotIn("was skipped", err)
        (release,) = self.github.releases.values()
        self.assertEqual((release["tag_name"], release["target_commitish"], release["prerelease"],
                          release["draft"], release["name"], release["body"]),
                         (TAG, HEAD, True, False, "Nightly 2026-10-29", DIGEST))
        self.assertEqual(self.github.tags, {TAG: HEAD})
        self.assertEqual([item["name"] for item in release["assets"]], ASSETS)
        uploads = [path for method, path in self.github.writes() if method == "POST"
                   and "/assets?" in path]
        self.assertEqual([urllib.parse.unquote(p.split("name=")[1]) for p in uploads], ASSETS)
        # Exactly one message, plain text: no parse mode, the link preview off.
        self.assertEqual(self.telegram.sent, [{
            "chat_id": -1001234567890, "text": DIGEST,
            "link_preview_options": {"is_disabled": True}}])
        self.assertEqual(self.runner.calls, [(
            ["minisign", "-V", "-q", "-m", "SHA256SUMS", "-x", "SHA256SUMS.minisig", "-p",
             self.path("nightly.pub")], os.path.join(self.night, "dist"))])
        receipt = self.receipt()
        self.assertEqual((receipt["status"], receipt["release"], receipt["telegram"],
                          receipt["telegram_reason"], receipt["amendments"], receipt["pruned"],
                          receipt["prune_failures"], receipt["foreign"],
                          receipt["published_unix_ms"], receipt["head_sha"]),
                         ("published", {"id": release["id"], "url": url}, {"message_id": 71},
                          None, 0, [], [], [], 1761710400000, record["head_sha"]))
        files = [os.path.join(self.night, "dist", ASSETS[0]),
                 os.path.join(self.night, "dist", ASSETS[1]),
                 os.path.join(self.night, "dist", "SHA256SUMS"),
                 os.path.join(self.night, "dist", "SHA256SUMS.minisig"),
                 os.path.join(self.night, "nightly.json"),
                 self.path("report", "daily.json"), self.path("report", "report.html")]
        self.assertEqual(receipt["assets"], [
            {"name": name, "size": len(read(path)), "digest": sha256(read(path))}
            for name, path in zip(ASSETS, files)])
        self.assertEqual([(item["name"], item["data"]) for item in release["assets"]],
                         [(name, read(path)) for name, path in zip(ASSETS, files)])

    def test_the_tokens_never_reach_an_argument_and_the_upload_stays_on_github(self):
        self.build()
        self.assertEqual(self.publish()[0], 0)
        self.assertFalse(any(WRITE_TOKEN in word or BOT_TOKEN in word
                             for argv, _ in self.runner.calls for word in argv))
        client = self.github.client()
        with self.assertRaises(githubrest.Refused) as caught:
            client.upload("https://evil.example/repos/owner/af/releases/1/assets{?name}", "x",
                          b"x", "text/plain")
        self.assertIn("not an asset upload of owner/af", str(caught.exception))
        with self.assertRaises(githubrest.Refused):
            githubrest.GitHub(REPO, None, send=self.github.send).release_by_tag(TAG)

    def test_a_tag_already_naming_another_commit_refuses_before_any_write(self):
        self.build()
        self.github.tags[TAG] = "c" * 40
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("the tag %s names %s on GitHub, not the night's head %s"
                      % (TAG, "c" * 40, HEAD), err)
        self.assertEqual(self.github.writes(), [])
        self.assertFalse(os.path.exists(os.path.join(self.night, "publish.json")))


class Amending(Night):
    def test_a_recomposed_report_replaces_the_notes_daily_json_and_report_html_only(self):
        self.build()
        self.assertEqual(self.publish()[0], 0)
        (release,) = self.github.releases.values()
        shipped = dict((item["name"], item["id"]) for item in release["assets"][:5])
        reported = dict((item["name"], item["id"]) for item in release["assets"][5:])
        self.github.requests = []
        report = self.report("late", daily=json.dumps({"schema": "af.nightly-daily/1",
                                                       "late": True}).encode(),
                             html=b"<html>with the late result</html>\n",
                             digest="nightly-2026.10.29: the late result is in\n")
        code, out, err = self.publish(report=report)
        self.assertEqual(code, 0, err)
        self.assertEqual(out, "https://github.com/owner/af/releases/tag/%s\n" % TAG)
        ids = dict((item["name"], item["id"]) for item in release["assets"])
        assets = "/repos/owner/af/releases/%d/assets?name=" % release["id"]
        self.assertEqual(self.github.writes(), [
            ("PATCH", "/repos/owner/af/releases/%d" % release["id"]),
            ("DELETE", "/repos/owner/af/releases/assets/%d" % reported["daily.json"]),
            ("POST", assets + "daily.json"),
            ("DELETE", "/repos/owner/af/releases/assets/%d" % reported["report.html"]),
            ("POST", assets + "report.html")])
        self.assertEqual(release["body"], "nightly-2026.10.29: the late result is in\n")
        # The shipped bytes are the same assets as before; the report is new.
        self.assertEqual(dict((name, ids[name]) for name in shipped), shipped)
        self.assertEqual([item["name"] for item in release["assets"]], ASSETS)
        self.assertEqual(release["assets"][6]["data"], b"<html>with the late result</html>\n")
        # No second message without --notify-again; the first one stays on the receipt.
        self.assertEqual(len(self.telegram.sent), 1)
        receipt = self.receipt()
        self.assertEqual((receipt["amendments"], receipt["telegram"]), (1, {"message_id": 71}))
        self.assertEqual(receipt["assets"][5]["digest"], sha256(read(os.path.join(report,
                                                                                  "daily.json"))))
        code, _, err = self.publish("--notify-again", report=report)
        self.assertEqual(code, 0, err)
        self.assertEqual([m["text"] for m in self.telegram.sent][1:],
                         ["nightly-2026.10.29: the late result is in\n"])
        self.assertEqual((self.receipt()["amendments"], self.receipt()["telegram"]),
                         (2, {"message_id": 72}))
        self.assertEqual(len(self.github.releases), 1)

    def test_a_shipped_archive_that_differs_refuses_before_any_write(self):
        self.build()
        self.assertEqual(self.publish()[0], 0)
        before = self.receipt()
        (release,) = self.github.releases.values()
        # The release carries an archive of other bytes than the night's.
        release["assets"][0]["digest"] = sha256(b"another build")
        self.github.requests = []
        code, _, err = self.publish(report=self.report("late"))
        self.assertEqual(code, 1)
        self.assertIn("GitHub reports %s with" % ASSETS[0], err)
        self.assertIn("not the night's", err)
        self.assertEqual(self.github.writes(), [])
        self.assertEqual(len(self.telegram.sent), 1)
        self.assertEqual(self.receipt(), before)
        # So does an archive changed in the night's directory, before any request at all.
        with open(os.path.join(self.night, "dist", ASSETS[1]), "ab") as stream:
            stream.write(b"tampered")
        self.github.requests = []
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("is not the archive nightly.json records", err)
        self.assertEqual(self.github.requests, [])

    def test_a_release_at_another_commit_or_not_a_pre_release_is_never_amended(self):
        self.build()
        self.github.add(TAG, sha="c" * 40)
        self.github.tags.pop(TAG)
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("targets %s, not the night's head %s" % ("c" * 40, HEAD), err)
        (release,) = self.github.releases.values()
        release.update(target_commitish=HEAD, prerelease=False)
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("is not a published pre-release: a foreign release, never amended", err)
        self.assertEqual(self.github.writes(), [])

    def test_a_retried_first_publish_completes_the_release_and_sends_the_message_once(self):
        self.build()
        self.github.fail = lambda method, path: "name=nightly.json" in path
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("GitHub answered HTTP 502 for POST", err)
        self.assertFalse(os.path.exists(os.path.join(self.night, "publish.json")))
        self.assertEqual(self.telegram.sent, [])
        self.github.fail = None
        code, _, err = self.publish()
        self.assertEqual(code, 0, err)
        (release,) = self.github.releases.values()
        self.assertEqual([item["name"] for item in release["assets"]], ASSETS)
        # No receipt means no message yet: the retry sends the digest, once.
        self.assertEqual([m["text"] for m in self.telegram.sent], [DIGEST])
        receipt = self.receipt()
        self.assertEqual((receipt["amendments"], receipt["telegram"],
                          receipt["telegram_reason"]), (1, {"message_id": 71}, None))
        # From now on the receipt holds the message: an amendment sends nothing.
        self.assertEqual(self.publish()[0], 0)
        self.assertEqual(len(self.telegram.sent), 1)


class SkippedNight(Night):
    def test_a_skipped_night_sends_one_line_and_publishes_nothing(self):
        record = self.skip()
        code, out, err = self.publish(date="2026-10-30", report=self.path("absent"))
        self.assertEqual(code, 0, err)
        self.assertEqual(out, "")
        self.assertEqual(self.github.requests, [])
        self.assertEqual([m["text"] for m in self.telegram.sent],
                         ["nightly-2026.10.30: no new commits on main since %s"
                          % record["previous_tag"]])
        receipt = self.receipt("2026-10-30")
        self.assertEqual((receipt["status"], receipt["release"], receipt["assets"],
                          receipt["telegram"], receipt["amendments"]),
                         ("skipped", None, [], {"message_id": 71}, 0))
        # A second publish of the skipped night sends nothing again, --notify-again included.
        code, _, err = self.publish(date="2026-10-30", report=self.path("absent"))
        self.assertEqual(code, 0, err)
        self.assertEqual(len(self.telegram.sent), 1)
        self.assertEqual(self.receipt("2026-10-30")["amendments"], 1)
        code, _, err = self.publish("--notify-again", date="2026-10-30",
                                    report=self.path("absent"))
        self.assertEqual(code, 0, err)
        self.assertEqual(len(self.telegram.sent), 1)
        receipt = self.receipt("2026-10-30")
        self.assertEqual((receipt["amendments"], receipt["telegram"]), (2, {"message_id": 71}))
        self.assertEqual(self.github.requests, [])

    def test_a_night_never_built_refuses(self):
        code, _, err = self.publish(date="2026-10-28")
        self.assertEqual(code, 1)
        self.assertIn("nightly: refused: %s is missing: the night 2026-10-28 was never built"
                      % os.path.join(self.builds, "2026-10-28", "nightly.json"), err)
        self.assertEqual((self.github.requests, self.telegram.sent), ([], []))


class Pruning(Night):
    def test_only_the_oldest_nightly_pre_releases_beyond_keep_nights_go(self):
        self.write_config(keep_nights=14)
        self.github.add("v0.13.0", prerelease=False, sha="d" * 40)
        self.github.add("nightly-2026.10.01", prerelease=False, sha="e" * 40)
        for day in range(14, 29):
            self.github.add("nightly-2026.10.%02d" % day, sha="%x" % (day % 16) * 40)
        self.github.add("nightly-2026.10.13-rc", sha="f" * 40)
        self.build()
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("nightly: refused: nightly-2026.10.01 is a nightly-named release that is "
                      "a draft or not a pre-release: foreign, never pruned", err)
        tags = sorted(r["tag_name"] for r in self.github.releases.values())
        self.assertEqual(tags, sorted(["v0.13.0", "nightly-2026.10.01", "nightly-2026.10.13-rc"]
                                      + ["nightly-2026.10.%02d" % d for d in range(16, 30)]))
        self.assertNotIn("nightly-2026.10.14", self.github.tags)
        self.assertNotIn("nightly-2026.10.15", self.github.tags)
        self.assertIn("nightly-2026.10.01", self.github.tags)
        self.assertIn("v0.13.0", self.github.tags)
        deletes = [(m, p) for m, p in self.github.writes() if m == "DELETE"]
        self.assertEqual([p.rsplit("/", 1)[1] if "tags" in p else "release" for _, p in deletes],
                         ["release", "nightly-2026.10.14", "release", "nightly-2026.10.15"])
        receipt = self.receipt()
        self.assertEqual((receipt["pruned"], receipt["foreign"]),
                         (["nightly-2026.10.14", "nightly-2026.10.15"], ["nightly-2026.10.01"]))
        # The release itself and the message are done; nothing more is pruned on a rerun.
        self.assertEqual(len(self.telegram.sent), 1)
        self.github.requests = []
        self.assertEqual(self.publish()[0], 1)
        self.assertEqual([m for m, p in self.github.writes() if m == "DELETE"
                          and "assets" not in p], [])

    def test_a_nightly_named_draft_pre_release_is_foreign_and_never_deleted(self):
        draft = self.github.add("nightly-2026.10.02", draft=True)
        for day in range(10, 14):
            self.github.add("nightly-2026.10.%02d" % day)
        self.write_config(keep_nights=3)
        self.build()
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("nightly: refused: nightly-2026.10.02 is a nightly-named release that is "
                      "a draft or not a pre-release: foreign, never pruned", err)
        self.assertIs(self.github.releases.get(draft["id"]), draft)
        self.assertIn("nightly-2026.10.02", self.github.tags)
        self.assertNotIn(("DELETE", "/repos/owner/af/releases/%d" % draft["id"]),
                         self.github.writes())
        self.assertNotIn(("DELETE", "/repos/owner/af/git/refs/tags/nightly-2026.10.02"),
                         self.github.writes())
        receipt = self.receipt()
        self.assertEqual((receipt["pruned"], receipt["prune_failures"], receipt["foreign"]),
                         (["nightly-2026.10.10", "nightly-2026.10.11"], [],
                          ["nightly-2026.10.02"]))

    def test_a_failure_while_pruning_keeps_the_release_message_and_receipt(self):
        for day in range(10, 14):
            self.github.add("nightly-2026.10.%02d" % day)
        self.write_config(keep_nights=3)
        self.build()
        # The oldest goes; the next one's release goes, then deleting its tag fails.
        self.github.fail = lambda method, path: (method == "DELETE"
                                                 and path.endswith("nightly-2026.10.11"))
        code, out, err = self.publish()
        self.assertEqual(code, 1)
        self.assertEqual(out, "https://github.com/owner/af/releases/tag/%s\n" % TAG)
        reason = ("GitHub answered HTTP 502 for DELETE /repos/owner/af/git/refs/tags/"
                  "nightly-2026.10.11: Bad gateway")
        self.assertIn("nightly: failed: pruning: the release of nightly-2026.10.11 is deleted "
                      "but its tag is not; delete the tag by hand: " + reason, err)
        receipt = self.receipt()
        # Only a tag whose ref is gone too counts as pruned; the other is recorded apart.
        self.assertEqual((receipt["pruned"], receipt["prune_failures"], receipt["telegram"]),
                         (["nightly-2026.10.10"],
                          [{"tag": "nightly-2026.10.11", "reason": reason}],
                          {"message_id": 71}))
        self.assertIsNone(self.github.by_tag("nightly-2026.10.11"))
        self.assertEqual(sorted(self.github.tags), ["nightly-2026.10.11", "nightly-2026.10.12",
                                                    "nightly-2026.10.13", TAG])
        self.assertEqual(len(self.telegram.sent), 1)

    def test_a_refused_tag_deletion_is_a_failure_not_a_pruned_tag(self):
        for day in range(10, 14):
            self.github.add("nightly-2026.10.%02d" % day)
        self.write_config(keep_nights=3)
        self.build()
        # GitHub answers 422 to the tag deletion: the ref may still be there, so it is not
        # pruned, and the receipt says so.
        self.github.fail = lambda method, path: 422 if (
            method == "DELETE" and path.endswith("nightly-2026.10.11")) else False
        code, out, err = self.publish()
        self.assertEqual(code, 1)
        receipt = self.receipt()
        self.assertEqual(receipt["pruned"], ["nightly-2026.10.10"])
        self.assertEqual([item["tag"] for item in receipt["prune_failures"]],
                         ["nightly-2026.10.11"])
        self.assertIn("HTTP 422", receipt["prune_failures"][0]["reason"])
        self.assertIn("nightly: failed: pruning: the release of nightly-2026.10.11 is deleted "
                      "but its tag is not", err)
        self.assertIn("nightly-2026.10.11", self.github.tags)

    def test_a_foreign_draft_is_named_even_when_pruning_fails_after_it(self):
        for day in range(10, 14):
            self.github.add("nightly-2026.10.%02d" % day)
        self.github.add("nightly-2026.10.05", draft=True)
        self.write_config(keep_nights=3)
        self.build()
        self.github.fail = lambda method, path: (method == "DELETE"
                                                 and path.endswith("nightly-2026.10.11"))
        code, out, err = self.publish()
        self.assertEqual(code, 1)
        receipt = self.receipt()
        self.assertEqual(receipt["foreign"], ["nightly-2026.10.05"])
        self.assertIn("nightly: refused: nightly-2026.10.05 is a nightly-named release that is "
                      "a draft or not a pre-release: foreign, never pruned", err)
        self.assertIsNotNone(self.github.by_tag("nightly-2026.10.05"))
        self.assertIn("nightly-2026.10.05", self.github.tags)

    def test_a_release_created_as_a_draft_gets_no_asset_and_no_message(self):
        self.write_config()
        self.build()
        self.github.create_draft = True
        code, out, err = self.publish()
        self.assertEqual(code, 1)
        self.assertEqual(out, "")
        self.assertIn("nightly: refused: GitHub holds the release of %s as something else "
                      "(a draft, another commit or not a pre-release); nothing was uploaded"
                      % TAG, err)
        self.assertEqual([r for r in self.github.requests if r[0] == "POST"
                          and "/assets" in r[1]], [])
        self.assertEqual(self.telegram.sent, [])
        self.assertFalse(os.path.exists(self.path("publish.json")))

    def test_an_older_night_beyond_keep_nights_is_pruned_too_and_keep_nights_is_bounded(self):
        for day in range(1, 5):
            self.github.add("nightly-2026.11.%02d" % day)
        self.write_config(keep_nights=3)
        self.build()
        code, out, err = self.publish()
        self.assertEqual(code, 0, err)
        self.assertEqual(out, "https://github.com/owner/af/releases/tag/%s\n" % TAG)
        self.assertIn("nightly: %s is older than the newest keep_nights nightly pre-releases: "
                      "published, then pruned" % TAG, err)
        # Exactly the newest three stay: the night just published went, release before tag.
        self.assertEqual(sorted(r["tag_name"] for r in self.github.releases.values()),
                         ["nightly-2026.11.02", "nightly-2026.11.03", "nightly-2026.11.04"])
        self.assertEqual(sorted(self.github.tags), ["nightly-2026.11.02", "nightly-2026.11.03",
                                                    "nightly-2026.11.04"])
        self.assertIn(("POST", "/repos/owner/af/releases"), self.github.writes())
        deletes = [p for m, p in self.github.writes() if m == "DELETE"]
        self.assertEqual([p.rsplit("/", 1)[1] if "tags" in p else "release" for p in deletes],
                         ["release", TAG, "release", "nightly-2026.11.01"])
        receipt = self.receipt()
        self.assertEqual((receipt["pruned"], receipt["prune_failures"]),
                         ([TAG, "nightly-2026.11.01"], []))
        self.write_config(keep_nights=2)
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("publish.keep_nights: must be within 3..60", err)


class Telegram(Night):
    def test_without_telegram_nothing_is_sent_and_the_receipt_says_why(self):
        self.write_config(telegram=False)
        self.build()
        code, _, err = self.publish()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.telegram.sent, [])
        receipt = self.receipt()
        self.assertEqual((receipt["telegram"], receipt["telegram_reason"]),
                         (None, publish.UNCONFIGURED))

    def test_a_failed_message_leaves_the_release_and_exits_1_with_the_receipt(self):
        self.telegram.answer = (400, b'{"ok": false, "description": "Bad Request: chat not '
                                     b'found"}')
        self.build()
        code, out, err = self.publish()
        self.assertEqual(code, 1)
        self.assertEqual(out, "https://github.com/owner/af/releases/tag/%s\n" % TAG)
        self.assertIn("nightly: failed: Telegram sendMessage failed: HTTP 400: Bad Request: "
                      "chat not found", err)
        self.assertEqual(len(self.github.releases), 1)
        receipt = self.receipt()
        self.assertEqual((receipt["telegram"], receipt["telegram_reason"]), (
            None, "Telegram sendMessage failed: HTTP 400: Bad Request: chat not found"))

    def test_an_unreadable_bot_token_refuses_before_any_request(self):
        os.remove(self.path("telegram"))
        self.build()
        code, _, err = self.publish()
        self.assertEqual(code, 1)
        self.assertIn("telegram_token_file %s" % self.path("telegram"), err)
        self.assertEqual(self.github.requests, [])


class Refusals(Night):
    def assert_refused(self, text, **options):
        code, _, err = self.publish(**options)
        self.assertEqual(code, 1)
        self.assertIn(text, err)
        self.assertEqual((self.github.requests, self.telegram.sent), ([], []))
        self.assertFalse(os.path.exists(os.path.join(self.night, "publish.json")))

    def test_a_signature_that_does_not_verify_refuses(self):
        self.build()
        self.runner.verifies = False
        self.assert_refused("SHA256SUMS.minisig does not verify against public_key")
        self.runner.verifies = True
        with open(os.path.join(self.night, "dist", "SHA256SUMS"), "a") as stream:
            stream.write("0" * 64 + "  extra.tar.gz\n")
        self.assert_refused("dist/SHA256SUMS does not match the night's archives")

    def test_a_missing_or_oversized_report_file_refuses(self):
        self.build()
        for name in ("daily.json", "report.html", "digest.md"):
            directory = self.report("missing-" + name)
            os.remove(os.path.join(directory, name))
            self.assert_refused("%s is missing" % os.path.join(directory, name),
                                report=directory)
        self.assert_refused("digest.md %s is larger than 3500 bytes" % self.path(
            "long", "digest.md"), report=self.report("long", digest="x" * 3501))
        self.assert_refused("report.html %s is larger than 4194304 bytes" % self.path(
            "huge", "report.html"), report=self.report("huge", html=b"x" * (4 * 1024 * 1024 + 1)))
        self.assert_refused("is no af.nightly-daily/* document", report=self.report(
            "other", daily=b'{"schema": "af.nightly-build/1"}'))
        self.assert_refused("digest.md is not UTF-8 text",
                            report=self.report("latin", digest=b"caf\xe9\n"))

    def test_a_configuration_without_publish_or_with_one_token_refuses(self):
        self.build()
        with open(self.config) as stream:
            text = stream.read()
        with open(self.config, "w") as stream:
            stream.write(text.split("[publish]")[0])
        self.assert_refused("has no [publish] table")
        self.write_config(telegram=False, telegram_chat_id="@afnightly")
        self.assert_refused("telegram_token_file and telegram_chat_id are set together")

    def test_the_command_line_refuses_with_exit_1(self):
        for argv in (["publish"], ["publish", "--config", self.config, "--date", "2026-13-01",
                                   "--report", self.tmp]):
            result = subprocess.run([sys.executable, "-I", "-B", PUBLISH] + argv,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertEqual(result.returncode, 1, argv)
            self.assertIn(b"nightly: refused: ", result.stderr)
            self.assertEqual(result.stdout, b"")


class ReportRoot(Night):
    """`--report-root` and the configuration's `reports_root`, and the night defaulted to
    today's UTC date (ADR-0272)."""

    def run_main(self, *argv, clock=1793286000000):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = publish.main(["publish", "--config", self.config] + list(argv),
                                github=self.github.client(), telegram_send=self.telegram.send,
                                run=self.runner, clock=lambda: clock)
        return code, out.getvalue(), err.getvalue()

    def test_the_newest_report_of_the_night_under_the_root_is_published(self):
        self.build()
        root = self.path("reports")
        for name in ("2026-10-29", "2026-10-29.2", "2026-10-29.10", "2026-10-30"):
            self.report(os.path.join("reports", name), digest="report %s\n" % name)
        os.mkdir(self.path("elsewhere"))
        os.symlink(self.path("elsewhere"), os.path.join(root, "2026-10-29.11"))
        code, out, err = self.run_main("--date", "2026-10-29", "--report-root", root)
        self.assertEqual(code, 0, err)
        newest = os.path.join(root, "2026-10-29.10")
        self.assertIn("nightly: --report %s (the newest of the night under %s)\n"
                      % (newest, root), err)
        (release,) = self.github.releases.values()
        self.assertEqual(release["body"], "report 2026-10-29.10\n")
        self.assertEqual(self.telegram.sent[0]["text"], "report 2026-10-29.10\n")
        self.assertEqual(self.receipt()["published_unix_ms"], 1793286000000)

    def test_without_date_or_report_the_night_and_root_are_the_defaults_and_said(self):
        self.build()
        root = self.path("reports")
        self.report(os.path.join("reports", "2026-10-29"))
        with open(self.config) as stream:
            text = stream.read()
        with open(self.config, "w") as stream:
            stream.write('reports_root = "%s"\n' % root + text)
        # 2026-10-29T15:00:00Z.
        code, out, err = self.run_main()
        self.assertEqual(code, 0, err)
        self.assertEqual(out, "https://github.com/owner/af/releases/tag/%s\n" % TAG)
        self.assertIn("nightly: --date 2026-10-29 (today's UTC date)\n", err)
        self.assertIn("nightly: --report-root %s (the configuration's reports_root)\n" % root,
                      err)
        self.assertEqual(self.github.releases[next(iter(self.github.releases))]["body"], DIGEST)

    def test_a_built_night_without_a_report_under_the_root_is_refused(self):
        self.build()
        root = self.path("reports")
        os.mkdir(root)
        self.report(os.path.join("reports", "2026-10-28"))
        code, out, err = self.run_main("--date", "2026-10-29", "--report-root", root)
        self.assertEqual((code, out), (1, ""))
        self.assertIn("nightly: refused: --report-root %s holds no report of the night "
                      "2026-10-29" % root, err)
        self.assertEqual((self.github.requests, self.telegram.sent), ([], []))
        self.assertFalse(os.path.exists(os.path.join(self.night, "publish.json")))
        code, _, err = self.run_main("--date", "2026-10-29")
        self.assertEqual(code, 1)
        self.assertIn("publish needs --report, --report-root or the configuration's "
                      "reports_root", err)

    def test_a_skipped_night_needs_no_report_under_the_root(self):
        self.skip()
        root = self.path("reports")
        os.mkdir(root)
        code, _, err = self.run_main("--date", "2026-10-30", "--report-root", root)
        self.assertEqual(code, 0, err)
        self.assertEqual(len(self.telegram.sent), 1)
        self.assertEqual(self.receipt("2026-10-30")["status"], "skipped")


class Receipt(Night):
    def test_the_receipt_contract_is_closed(self):
        self.build()
        self.assertEqual(self.publish()[0], 0)
        doc = json.loads(read(os.path.join(self.night, "publish.json")))
        for change, error in ((dict(assets=doc["assets"][::-1]), "in order"),
                              (dict(telegram=None), "the reason there is none"),
                              (dict(status="skipped"), "a skipped night publishes"),
                              (dict(schema="af.nightly-publish/2"),
                               "expected af.nightly-publish/1"),
                              (dict(token="x"), "unknown field token")):
            with self.assertRaises(ContractError, msg=error) as caught:
                contracts.read_json(json.dumps(dict(doc, **change)).encode(), "nightly-publish")
            self.assertIn(error, str(caught.exception))
