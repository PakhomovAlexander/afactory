"""The trusted collector's Actions transport (package p4, ADR-0172).

A fake transport stands for the GitHub API and its signed artifact storage on another origin;
no network is reached and the token is a literal non-secret marker. The artifacts are real ZIP
containers of current-format synthetic runs, labeled synthetic, in the af.perf-transport/1
format the collector reads. The HTTPS transport itself is exercised against owned localhost TLS
servers with a certificate made for the test, its verification on (ADR-0173).
"""
import io
import json
import os
import shutil
import socket
import ssl
import stat
import subprocess
import threading
import time
import warnings
import zipfile

from perflab import archive, builds, history, runner, supervise, synthetic, transport
from perflab.values import ContractError
from tests.support import TempDir, read, run_dir, synthetic_suite

TOKEN = "marker-token-not-a-secret"
API = "https://api.example.invalid"
STORAGE = "https://storage.example.invalid"
REPOSITORY = "example/af"
ACTIONS_RUN = 4242
T0 = 1800000000000
SCRIPTED_VIEW = (b'<!DOCTYPE html><html><head><meta http-equiv="Content-Security-Policy" '
                 b'content="script-src \'sha256-AAAA\'"></head><body><script>'
                 b'document.title="transported code ran"</script></body></html>')


class FakeTransport(object):
    """Answers like the API (listing pages, a redirect per artifact download) and signed
    storage (the ZIP bytes); records every request it receives."""

    def __init__(self, artifacts, page_size=2):
        self.artifacts = artifacts
        self.page_size = page_size
        self.requests = []
        self.answers = {}

    def request(self, url, headers, limit, timeout):
        self.requests.append((url, dict(headers)))
        if url in self.answers:
            return self.answers[url]
        if url.startswith(API + "/repos/%s/actions/runs/%d/artifacts" % (REPOSITORY,
                                                                          ACTIONS_RUN)):
            page = int(url.rsplit("page=", 1)[1])
            items = self.artifacts[(page - 1) * self.page_size:page * self.page_size]
            listed = [dict((key, value) for key, value in item.items() if key != "data")
                      for item in items]
            body = json.dumps({"total_count": len(self.artifacts),
                               "artifacts": listed}).encode()
            return transport.Response(200, {}, body)
        for item in self.artifacts:
            if url == API + "/repos/%s/actions/artifacts/%d/zip" % (REPOSITORY, item["id"]):
                if item.get("gone"):
                    return transport.Response(410, {}, b"")
                return transport.Response(302, {"Location": "%s/blob/%d?sig=SIGNED%d"
                                                % (STORAGE, item["id"], item["id"])}, b"")
            if url == "%s/blob/%d?sig=SIGNED%d" % (STORAGE, item["id"], item["id"]):
                data = item["data"]
                return transport.Response(200, {}, data[:limit + 1])
        return transport.Response(404, {}, b"")


def artifact(ident, name, data, expired=False, expires="2027-01-20T00:00:00Z", gone=False):
    return {"id": ident, "name": name, "size_in_bytes": len(data), "expired": expired,
            "created_at": "2026-10-20T00:00:00Z", "expires_at": expires,
            "workflow_run": {"id": ACTIONS_RUN}, "data": data, "gone": gone}


def zipped(members, symlink=None):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as container:
        for name, data in members:
            with warnings.catch_warnings():
                # A duplicate member is the hostile container under test.
                warnings.simplefilter("ignore", UserWarning)
                container.writestr(name, data)
        if symlink is not None:
            info = zipfile.ZipInfo(symlink)
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            container.writestr(info, "/etc/passwd")
    return buffer.getvalue()


class Collector(TempDir):
    def setUp(self):
        TempDir.setUp(self)
        self.suite = synthetic_suite()
        plan = synthetic.plan(self.suite, run_id="fetched_run")
        self.run = run_dir(self, plan, synthetic.constant("10.5"))
        self.root = self.path("archive")
        archive.Archive.create(self.root, wall=lambda: T0 / 1000.0)

    def transports(self, views=()):
        """The run's three transport artifacts as a workflow would upload them."""
        found = []
        for index, (content, shard) in enumerate((("run_bundle", None), ("shard_bundle", 0),
                                                  ("shard_bundle", 1))):
            path = self.path("transport-%d.zip" % index)
            transport.write_transport(path, self.run, content, shard,
                                      views if content == "run_bundle" else ())
            found.append(artifact(1001 + index, "af-perf-%s-%s" % (content, shard),
                                  read(path)))
        return found

    def fetch(self, artifacts, lab_run=None, **options):
        fake = FakeTransport(artifacts, **options)
        collector = transport.Collector(fake, REPOSITORY, TOKEN, api_url=API)
        with archive.Archive(self.root, wall=lambda: T0 / 1000.0) as store:
            try:
                return transport.fetch(store, collector, ACTIONS_RUN, lab_run), fake
            finally:
                self.collections = store.collections()

    def assert_token_confined(self, fake):
        for url, headers in fake.requests:
            if url.startswith(API + "/"):
                self.assertEqual(headers.get("Authorization"), "Bearer " + TOKEN)
            else:
                self.assertTrue(url.startswith(STORAGE + "/"), url)
                self.assertNotIn("Authorization", headers)
                self.assertNotIn(TOKEN, json.dumps(headers))
        for current, _, names in os.walk(self.root):
            for name in names:
                data = read(os.path.join(current, name))
                self.assertNotIn(TOKEN.encode(), data, name)
                self.assertNotIn(b"SIGNED", data, name)
                self.assertNotIn(STORAGE.encode(), data, name)

    def test_a_run_is_collected_with_the_token_confined_to_the_api_origin(self):
        (status, run_id), fake = self.fetch(self.transports())
        self.assertEqual((status, run_id), ("imported", "fetched_run"))
        self.assert_token_confined(fake)
        urls = [url for url, _ in fake.requests]
        # Two listing pages of two, then each artifact's redirect and its signed download.
        self.assertEqual(urls[:2], [
            API + "/repos/example/af/actions/runs/4242/artifacts?per_page=100&page=1",
            API + "/repos/example/af/actions/runs/4242/artifacts?per_page=100&page=2"])
        self.assertEqual(len(urls), 2 + 3 * 2)
        collection = self.collections[-1]
        self.assertEqual(collection["status"], "imported")
        self.assertEqual([item["state"] for item in collection["source"]["artifacts"]],
                         ["downloaded"] * 3)
        with archive.Archive(self.root) as store:
            source = store.state()["fetched_run"].event["event"]["source"]
            self.assertEqual((source["kind"], source["repository"], source["actions_run_id"]),
                             ("actions", REPOSITORY, ACTIONS_RUN))
            rebuilt = store.materialize("fetched_run", self.path("rebuilt"))
        self.assertEqual(sorted(os.listdir(rebuilt)), sorted(os.listdir(self.run)))
        # The same fetch again is idempotent.
        (status, _), _ = self.fetch(self.transports())
        self.assertEqual(status, "already_archived")

    def test_one_laboratory_run_of_a_continuous_workflow_run_is_selected_by_name(self):
        # The continuous workflow names each artifact af-perf-<run>-run or -shard-<i>
        # (ADR-0177); one Actions run may hold a primary and bootstrap runs.
        other = run_dir(self, synthetic.plan(self.suite, run_id="other_run"),
                        synthetic.constant("10.5"))
        found = []
        for run, run_id in ((self.run, "fetched_run"), (other, "other_run")):
            for content, shard, suffix in (("run_bundle", None, "run"),
                                           ("shard_bundle", 0, "shard-0"),
                                           ("shard_bundle", 1, "shard-1")):
                path = self.path("%s-%s.zip" % (run_id, suffix))
                transport.write_transport(path, run, content, shard)
                found.append(artifact(2001 + len(found), "af-perf-%s-%s" % (run_id, suffix),
                                      read(path)))
        with self.assertRaises(archive.Refused) as caught:
            self.fetch(found)
        self.assertIn("more than one laboratory run", str(caught.exception))
        (status, run_id), _ = self.fetch(found, lab_run="fetched_run")
        self.assertEqual((status, run_id), ("imported", "fetched_run"))
        self.assertEqual([item["name"] for item in self.collections[-1]["source"]["artifacts"]],
                         ["af-perf-fetched_run-run", "af-perf-fetched_run-shard-0",
                          "af-perf-fetched_run-shard-1"])

    def test_expired_and_missing_artifacts_leave_retained_evidence_incomplete(self):
        artifacts = self.transports()
        artifacts[2]["expired"] = True
        artifacts[1]["gone"] = True
        (status, run_id), fake = self.fetch(artifacts)
        self.assertEqual(status, "imported")
        with archive.Archive(self.root) as store:
            event = store.state()[run_id].event["event"]
            self.assertEqual(event["verdict"]["result"], "incomplete")
            self.assertEqual([item["bundle"] is None for item in event["bundles"]["shards"]],
                             [True, True])
            states = [item["state"] for item in event["source"]["artifacts"]]
            self.assertEqual(states, ["downloaded", "expired", "expired"])
            status = store.status(T0)
        # Expired transport means incomplete evidence, and the archive's health says so.
        self.assertEqual(status["health"]["status"], "failed")
        self.assertEqual([(entry["artifact_id"], entry["risk"]) for entry in status["transport"]],
                         [(1002, "expired"), (1003, "expired")])
        self.assertEqual(sorted(set(item["code"] for item in status["health"]["problems"])),
                         ["transport_expired"])
        # A failed collection's uncollected artifacts become a risk as their expiry nears.
        artifacts = self.transports()
        artifacts[1]["data"] = b"not a zip"
        artifacts[1]["size_in_bytes"] = len(b"not a zip")
        self.root = self.path("archive-2")
        archive.Archive.create(self.root, wall=lambda: T0 / 1000.0)
        with self.assertRaises(archive.Refused):
            self.fetch(artifacts)
        self.assertEqual([item["state"] for item in self.collections[-1]["source"]["artifacts"]],
                         ["downloaded", "refused", "not_collected"])
        with archive.Archive(self.root) as store:
            expires = transport._unix_ms("2027-01-20T00:00:00Z")
            status = store.status(expires - 30 * archive.DAY_MS)
            self.assertEqual(status["transport"], [])
            self.assertEqual([item["code"] for item in status["health"]["problems"]],
                             ["collection_failed"])
            status = store.status(expires - 2 * archive.DAY_MS)
            self.assertEqual([(entry["artifact_id"], entry["risk"])
                              for entry in status["transport"]],
                             [(1001, "expiring"), (1002, "expiring"), (1003, "expiring")])
            self.assertIn("transport_expiring",
                          [item["code"] for item in status["health"]["problems"]])
            status = store.status(expires + 1)
            self.assertEqual(set(entry["risk"] for entry in status["transport"]), {"expired"})
        # Once collected, nothing is at risk.
        (status, _), _ = self.fetch(self.transports())
        self.assertEqual(status, "imported")
        with archive.Archive(self.root) as store:
            status = store.status(expires - 2 * archive.DAY_MS)
            self.assertEqual(status["transport"], [])
            self.assertEqual(status["health"], {"status": "ok", "problems": []})

    def test_unknown_formats_and_unsafe_containers_are_refused_clearly(self):
        good = self.transports()
        cases = [
            (b"not a zip at all", "is not a ZIP container"),
            (zipped([("bundle/run.json", b"{}")]), "declares no af-perf-transport.json"),
            (zipped([("af-perf-transport.json", b'{"schema":"af.perf-transport/9"}'),
                     ("bundle/run.json", b"{}")]), "is not af.perf-transport/1"),
            (zipped([("af-perf-transport.json", b"{}"), ("../escape", b"x")]),
             "path segment '..' is not allowed"),
            (zipped([("af-perf-transport.json", b"{}"), ("/etc/x", b"x")]),
             "a path is relative"),
            (zipped([("af-perf-transport.json", b"{}"), ("bundle/a", b"x"),
                     ("bundle/a", b"y")]), "holds bundle/a twice"),
            (zipped([("af-perf-transport.json", b"{}")], symlink="bundle/link"),
             "is not a regular file"),
            (zipped([("af-perf-transport.json", b"{}"), ("other/x", b"x")]),
             "not part of the transport format"),
            (zipped([("af-perf-transport.json", b"{}"),
                     ("bundle/bomb", b"\0" * (8 * 1024 * 1024))]),
             "decompresses beyond the 200:1 ratio bound"),
        ]
        for data, why in cases:
            with self.subTest(why=why):
                artifacts = [artifact(2001, "af-perf-run_bundle-None", data)] + good[1:]
                with self.assertRaises(archive.Refused) as caught:
                    self.fetch(artifacts)
                self.assertIn(why, str(caught.exception))
                self.assertEqual(self.collections[-1]["status"], "transport_failed")
                self.assertEqual(self.collections[-1]["source"]["artifacts"][0]["state"],
                                 "refused")
        with archive.Archive(self.root) as store:
            self.assertEqual(store.events(), [])
            self.assertEqual(store.objects(), [])

    def test_redirects_origins_and_listings_are_bounded(self):
        artifacts = self.transports()
        fake = FakeTransport(artifacts)
        collector = transport.Collector(fake, REPOSITORY, TOKEN, api_url=API)
        url = API + "/repos/example/af/actions/artifacts/1001/zip"
        fake.answers[url] = transport.Response(302, {"Location": "http://storage.example."
                                                     "invalid/blob?sig=SIGNED"}, b"")
        with self.assertRaises(ContractError) as caught:
            collector.download(dict(id=1001, size=10))
        self.assertIn("is not an https URL", str(caught.exception))
        self.assertNotIn("SIGNED", str(caught.exception))
        fake.answers[url] = transport.Response(302, {"Location": API + "/loop"}, b"")
        fake.answers[API + "/loop"] = transport.Response(302, {"Location": API + "/loop"}, b"")
        with self.assertRaises(ContractError) as caught:
            collector.download(dict(id=1001, size=10))
        self.assertIn("redirected more than 3 times", str(caught.exception))
        # A redirect back to the API origin is followed without the credential too.
        self.assertTrue(all("Authorization" not in headers for request_url, headers
                            in fake.requests if request_url == API + "/loop"))
        with self.assertRaises(ContractError):
            collector.download(dict(id=1002, size=transport.ARTIFACT_BYTES_MAX + 1))
        for repository in ("example", "../x/y", "example/af/extra", "a b/c"):
            with self.assertRaises(ContractError):
                transport.Collector(fake, repository, TOKEN, api_url=API)
        for api in ("http://api.example.invalid", "https://api.example.invalid/other",
                    "https://api.example.invalid?x=1"):
            with self.assertRaises(ContractError):
                transport.Collector(fake, REPOSITORY, TOKEN, api_url=api)
        foreign = self.transports()
        foreign[0]["workflow_run"] = {"id": 1}
        with self.assertRaises(archive.Refused) as caught:
            self.fetch(foreign)
        self.assertIn("belongs to another run", str(caught.exception))
        # Listing pages are bounded: 1000 artifacts at most.
        many = [artifact(5000 + index, "other-%d" % index, b"") for index in range(1100)]
        collector = transport.Collector(FakeTransport(many, page_size=100), REPOSITORY, TOKEN,
                                        api_url=API)
        with self.assertRaises(ContractError) as caught:
            collector.artifacts(ACTIONS_RUN)
        self.assertIn("the listing named 1100 artifacts, more than the 1000 a collection "
                      "reads", str(caught.exception))
        # The whole collection is bounded in time.
        now = [0.0]
        slow = transport.Collector(FakeTransport(artifacts), REPOSITORY, TOKEN, api_url=API,
                                   clock=lambda: now[0], seconds=10)
        now[0] = 11.0
        with self.assertRaises(ContractError) as caught:
            slow.artifacts(ACTIONS_RUN)
        self.assertIn("the collection's 10-second bound ended before the next request",
                      str(caught.exception))

    def test_a_transported_scripted_view_is_kept_only_as_data(self):
        (status, run_id), _ = self.fetch(self.transports(views=[("report.html",
                                                                 SCRIPTED_VIEW)]))
        self.assertEqual(status, "imported")
        output = self.path("history")
        doc = history.write([self.root], output)
        run = [item for item in doc["runs"] if item["run_id"] == run_id][0]
        self.assertEqual([view["name"] for view in run["views"]], ["report.html"])
        view = run["views"][0]
        self.assertTrue(view["path"].startswith("downloads/") and view["path"].endswith(".bin"))
        self.assertEqual(read(os.path.join(output, *view["path"].split("/"))), SCRIPTED_VIEW)
        page = read(os.path.join(output, history.HISTORY_HTML)).decode()
        report_page = read(os.path.join(output, run["report"])).decode()
        for text in (page, report_page):
            self.assertNotIn("transported code ran", text)
            self.assertNotIn("sha256-AAAA", text)
            self.assertEqual(text.count("<script"), 1)
        self.assertIn('href="%s" download' % view["path"], page)

    def test_benchmarks_never_receive_the_collector_s_credential(self):
        os.environ[transport.TOKEN_ENV] = TOKEN
        os.environ["GITHUB_TOKEN"] = TOKEN
        try:
            self.assertEqual(transport.token_from(os.environ), TOKEN)
            case = [item for item in synthetic.plan(self.suite)["cases"]
                    if item["kind"] == "process"][0]
            environments = [runner.Shard.cli_environment(None, case, "/scratch"),
                            supervise.probe_environment("/scratch"),
                            builds.git_environment("/home")]
            for environment in environments:
                self.assertNotIn(TOKEN, json.dumps(environment))
                self.assertNotIn(transport.TOKEN_ENV, environment)
                self.assertNotIn("GITHUB_TOKEN", environment)
        finally:
            os.environ.pop(transport.TOKEN_ENV, None)
            os.environ.pop("GITHUB_TOKEN", None)
        with self.assertRaises(ContractError):
            transport.token_from({}, transport.TOKEN_ENV)
        with self.assertRaises(ContractError):
            transport.token_from({"bad name": TOKEN}, "bad name")


class LocalTls(object):
    """An owned HTTPS server on localhost: `answer(path)` gives the (delay, bytes) pieces it
    sends, each after its delay; every request's path and headers are recorded."""

    def __init__(self, certificate, key, answer):
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(certificate, key)
        self.answer = answer
        self.requests = []
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(8)
        self.port = self.listener.getsockname()[1]
        thread = threading.Thread(target=self.serve)
        thread.daemon = True
        thread.start()

    def url(self, path):
        return "https://localhost:%d%s" % (self.port, path)

    def serve(self):
        while True:
            try:
                connection, _ = self.listener.accept()
            except OSError:
                return
            thread = threading.Thread(target=self.handle, args=(connection,))
            thread.daemon = True
            thread.start()

    def handle(self, connection):
        stream = None
        try:
            stream = self.context.wrap_socket(connection, server_side=True)
            data = b""
            while b"\r\n\r\n" not in data:
                piece = stream.recv(4096)
                if not piece:
                    return
                data += piece
            lines = data.split(b"\r\n\r\n")[0].decode("latin-1").split("\r\n")
            path = lines[0].split(" ")[1]
            self.requests.append((path, dict(line.split(": ", 1) for line in lines[1:])))
            for delay, piece in self.answer(path):
                time.sleep(delay)
                stream.sendall(piece)
        except (OSError, ValueError):
            pass
        finally:
            (stream or connection).close()

    def close(self):
        self.listener.close()


def trickled(head, pieces, delay=0.4):
    """A response whose `head` is sent at once and whose `pieces` follow `delay` apart."""
    return [(0.0, head)] + [(delay, piece) for piece in pieces]


class HttpsDeadline(TempDir):
    """The HTTPS transport's one absolute deadline over real localhost TLS (ADR-0173):
    connection, headers, body and redirects, with certificate and host name verification."""

    def setUp(self):
        TempDir.setUp(self)
        openssl = shutil.which("openssl") or (
            "/usr/bin/openssl" if os.path.exists("/usr/bin/openssl") else None)
        if openssl is None:
            self.skipTest("no openssl here to make the test's localhost certificate")
        self.certificate, self.key = self.path("localhost.pem"), self.path("localhost.key")
        subprocess.run([openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout",
                        self.key, "-out", self.certificate, "-days", "1", "-subj",
                        "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True,
                       timeout=120)
        self.transport = transport.HttpsTransport(
            ssl.create_default_context(cafile=self.certificate))

    def server(self, answer):
        server = LocalTls(self.certificate, self.key, answer)
        self.addCleanup(server.close)
        return server

    def refused_on_time(self, call, bound):
        """`call` refused by a TransportError within `bound` and the cleanup allowance."""
        started = time.monotonic()
        with self.assertRaises(transport.TransportError) as caught:
            call()
        elapsed = time.monotonic() - started
        self.assertLess(elapsed, bound + transport.CLEANUP_SECONDS, elapsed)
        return caught.exception, elapsed

    def test_a_verified_response_is_read_whole(self):
        server = self.server(lambda path: [(0.0, b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n"
                                                 b"X-Seen: yes\r\n\r\nhello")])
        response = self.transport.request(server.url("/ok?x=1"), {"Accept": "a/b"}, 1024, 10)
        self.assertEqual((response.status, response.headers["x-seen"], response.body),
                         (200, "yes", b"hello"))
        self.assertEqual(server.requests[0][0], "/ok?x=1")
        self.assertEqual(server.requests[0][1]["Accept"], "a/b")

    def test_a_trickling_body_is_refused_at_the_deadline(self):
        # Root-reproduced: four body bytes 0.4 s apart under a 1 s bound used to be refused
        # only after 1.645 s, once the read that waited for them returned. Eight bytes take
        # 3.2 s here; the request is refused at its 1 s deadline.
        server = self.server(lambda path: trickled(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n", [b"x"] * 8))
        error, elapsed = self.refused_on_time(
            lambda: self.transport.request(server.url("/slow"), {}, 1024, 1.0), 1.0)
        self.assertGreaterEqual(elapsed, 0.9)
        self.assertIn("within its 1-second bound", str(error))

    def test_trickling_headers_are_refused_at_the_deadline(self):
        server = self.server(lambda path: trickled(
            b"HTTP/1.1 200 OK\r\n", [b"X-Slow-%d: y\r\n" % index for index in range(8)]
            + [b"Content-Length: 0\r\n\r\n"]))
        error, _ = self.refused_on_time(
            lambda: self.transport.request(server.url("/slow"), {}, 1024, 0.75), 0.75)
        # A subsecond bound is shown as it is, never truncated to 0.
        self.assertIn("within its 0.75-second bound", str(error))

    def test_certificates_and_host_names_are_still_verified(self):
        server = self.server(lambda path: [(0.0, b"HTTP/1.1 200 OK\r\nContent-Length: 0"
                                                 b"\r\n\r\n")])
        with self.assertRaises(ssl.SSLCertVerificationError):
            transport.HttpsTransport().request(server.url("/"), {}, 1024, 10)
        with self.assertRaises(ssl.SSLCertVerificationError):
            self.transport.request("https://127.0.0.1:%d/" % server.port, {}, 1024, 10)
        self.assertEqual(server.requests, [])

    def test_a_redirect_shares_the_collection_s_deadline_and_never_its_token(self):
        # The API answers its redirect after 0.6 s and signed storage its headers after 0.6 s
        # more: each within a 1 s bound alone, together beyond the collection's 1 s.
        storage = self.server(lambda path: [(0.6, b"HTTP/1.1 200 OK\r\nContent-Length: 2"
                                                  b"\r\n\r\nPK")])
        api = self.server(lambda path: [(0.6, ("HTTP/1.1 302 Found\r\nLocation: %s\r\n"
                                               "Content-Length: 0\r\n\r\n"
                                               % storage.url("/blob?sig=SIGNED")).encode())])
        collector = transport.Collector(self.transport, REPOSITORY, TOKEN,
                                        api_url="https://localhost:%d" % api.port, seconds=1.0)
        error, _ = self.refused_on_time(lambda: collector.download(dict(id=1001, size=10)),
                                        1.0)
        self.assertNotIn("SIGNED", str(error))
        self.assertEqual(api.requests[0][1]["Authorization"], "Bearer " + TOKEN)
        for _, headers in storage.requests:
            self.assertNotIn("Authorization", headers)
            self.assertNotIn(TOKEN, json.dumps(headers))
        # A trickling signed download is refused at the collection's deadline as well.
        slow = self.server(lambda path: trickled(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n", [b"x"] * 8))
        api = self.server(lambda path: [(0.0, ("HTTP/1.1 302 Found\r\nLocation: %s\r\n"
                                               "Content-Length: 0\r\n\r\n"
                                               % slow.url("/blob?sig=SIGNED")).encode())])
        collector = transport.Collector(self.transport, REPOSITORY, TOKEN,
                                        api_url="https://localhost:%d" % api.port, seconds=1.0)
        self.refused_on_time(lambda: collector.download(dict(id=1001, size=10)), 1.0)
        self.assertNotIn("Authorization", slow.requests[0][1])


class LateAdmission(TempDir):
    def test_a_response_after_the_collection_s_deadline_is_not_admitted(self):
        now = [0.0]

        class Late(object):
            def request(self, url, headers, limit, timeout):
                now[0] += 2.0
                return transport.Response(200, {}, b"{}")

        collector = transport.Collector(Late(), REPOSITORY, TOKEN, api_url=API,
                                        clock=lambda: now[0], seconds=1.5)
        with self.assertRaises(transport.TransportError) as caught:
            collector.artifacts(ACTIONS_RUN)
        self.assertIn("a response arrived after the collection's 1.5-second bound",
                      str(caught.exception))
